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
//! "hookSpecificOutput": {"permissionDecision": "allow"|"deny"|"ask"}}`; a
//! `PermissionRequest` hook answers with `hookSpecificOutput.decision`
//! (`{"behavior": "allow", "always": true}` answers Always).
//!
//! Events: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
//! `UserPromptSubmit`, `PermissionRequest`, `Stop`, `SessionStart`,
//! `SessionEnd`, `TurnEnd`, `Notification`, `FileChanged` and `ConfigChange`
//! defined by this module. Matchers compare against the tool name
//! for tool events and `FileChanged`, the `source`/`reason` for session
//! events and the `notification_type` for notifications.
//!
//! Diagnostics never log the configured command line: assignments and
//! arguments can carry credentials. Log lines name the program word only, and
//! any free text taken from a hook (stderr, block reasons) is redacted,
//! stripped of control characters and clipped first.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use haarness_core::shell_text::{ShellKind, place_placeholders};
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
                (Some(matcher), Some(tool)) => {
                    matcher_matches(matcher, tool)
                        || (event == "StopFailure"
                            && PROVIDER_FAILURES.contains(&tool)
                            && matcher_matches(matcher, "provider"))
                }
            })
            .collect()
    }

    async fn run(&self, entry: &HookEntry, payload: &Value) -> HookOutcome {
        use tokio::io::AsyncWriteExt as _;
        use tokio::process::Command;
        let (program, arguments) = hook_invocation(entry, &self.workspace);
        let mut command = Command::new(program);
        command.args(arguments);
        for name in haarness_core::private_env_names() {
            command.env_remove(name);
        }
        command
            .current_dir(dunce::simplified(&self.workspace))
            .env("HAARNESS_PROJECT_DIR", &self.workspace)
            .env("OPENCODE_PROJECT_DIR", &self.workspace)
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
        let label = HookLabel::new(entry, payload);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(
                    event = %label.event,
                    session = label.session.as_deref(),
                    tool = label.tool.as_deref(),
                    program = %label.program,
                    %error,
                    "hook failed to spawn"
                );
                return HookOutcome::default();
            }
        };
        // The payload is written while the output is read, both within the
        // hook's timeout. Written first, a payload larger than the pipe holds
        // waited, with no limit, for a hook that never reads it, and for one
        // that writes much before reading, which waited for us in turn. A hook
        // that exits without reading closes the pipe; that error is ignored,
        // and once the hook is done nothing waits for the rest to be written.
        let stdin = child.stdin.take();
        let input = payload.to_string().into_bytes();
        let finished = async move {
            let feed = async move {
                if let Some(mut stdin) = stdin {
                    let _ = stdin.write_all(&input).await;
                }
            };
            let wait = child.wait_with_output();
            tokio::pin!(wait);
            tokio::select! {
                output = &mut wait => output,
                () = feed => wait.await,
            }
        };
        if entry.asynchronous {
            // Nothing waits for an async hook's answer, so its failure is
            // only ever visible in this log line.
            let timeout = entry.timeout;
            tokio::spawn(async move {
                match tokio::time::timeout(timeout, finished).await {
                    Ok(Ok(output)) if !output.status.success() => {
                        label.failed(output.status, &output.stderr, true);
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => label.wait_failed(&error, true),
                    Err(_) => label.timed_out(timeout, true),
                }
            });
            return HookOutcome::default();
        }
        let output = match tokio::time::timeout(entry.timeout, finished).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                label.wait_failed(&error, false);
                return HookOutcome::default();
            }
            Err(_) => {
                label.timed_out(entry.timeout, false);
                return HookOutcome::default();
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut outcome = HookOutcome::default();
        // Exit code 2 is the Claude "block" convention; stderr is the reason.
        if output.status.code() == Some(2) {
            let reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let reason = if reason.is_empty() {
                "blocked by hook".into()
            } else {
                reason
            };
            label.blocked(output.status.code(), &reason);
            outcome.block = Some(reason);
            return outcome;
        }
        // Any other non-zero exit is a non-blocking error under the Claude
        // contract: the action proceeds, and this line says why the hook failed.
        if !output.status.success() {
            label.failed(output.status, &output.stderr, false);
        }
        let mut deny_reason = None;
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
                deny_reason = specific
                    .get("permissionDecisionReason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(context) = specific.get("additionalContext").and_then(Value::as_str) {
                    outcome.context = Some(context.to_owned());
                }
                if let Some(input) = specific.get("updatedInput").and_then(Value::as_object) {
                    outcome.updated_input = Some(Value::Object(input.clone()));
                }
                // Claude Code's PermissionRequest answer carries its rewrite
                // inside the decision. The runtime reads it only to know that
                // an allow was for other input (see `permission_hook_answer`).
                if entry.event == "PermissionRequest"
                    && let Some(input) = specific
                        .get("decision")
                        .and_then(|decision| decision.get("updatedInput"))
                        .and_then(Value::as_object)
                {
                    outcome.updated_input = Some(Value::Object(input.clone()));
                }
                match specific
                    .get("decision")
                    .and_then(|decision| decision.get("behavior"))
                    .and_then(Value::as_str)
                {
                    Some("allow") => {
                        outcome.permission = Some(PermissionDecision::Allow);
                        outcome.always = entry.event == "PermissionRequest"
                            && specific.get("decision").is_some_and(answers_always);
                    }
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
        // Only a successful hook can approve, rewrite input or add context
        // (Claude reads its JSON on exit 0); a crashed one may still refuse.
        if !output.status.success() {
            if outcome.permission != Some(PermissionDecision::Deny) {
                outcome.permission = None;
            }
            outcome.updated_input = None;
            outcome.context = None;
            outcome.always = false;
        }
        // A permission deny stops the action only where the runtime reads it.
        let denied = outcome.permission == Some(PermissionDecision::Deny)
            && matches!(entry.event.as_str(), "PreToolUse" | "PermissionRequest");
        if outcome.block.is_some() || denied {
            let reason = outcome
                .block
                .as_deref()
                .or(deny_reason.as_deref())
                .unwrap_or("denied by hook");
            label.blocked(output.status.code(), reason);
        }
        outcome
    }

    async fn run_event(&self, event: &str, tool: Option<&str>, payload: &Value) -> HookOutcome {
        let mut combined = HookOutcome::default();
        // Always holds only when every hook that allowed answered Always.
        let mut always = None;
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
            if outcome.permission == Some(PermissionDecision::Allow) {
                always = Some(always.unwrap_or(true) && outcome.always);
            }
            if outcome.updated_input.is_some() {
                combined.updated_input = outcome.updated_input;
            }
        }
        combined.always =
            combined.permission == Some(PermissionDecision::Allow) && always == Some(true);
        combined
    }
}

/// The program a hook runs and its arguments. A shell reads the placeholders
/// from the environment the hook gets (a POSIX shell's `${CLAUDE_PROJECT_DIR}`,
/// PowerShell's `${env:CLAUDE_PROJECT_DIR}`), so a folder name holding `$(…)`,
/// a quote or a space is never read as shell syntax, as it would be if pasted
/// into the command text. A hook with an `args` list runs without a shell and
/// gets the values filled in.
fn hook_invocation(entry: &HookEntry, workspace: &std::path::Path) -> (String, Vec<String>) {
    let expand = |value: &str| expand_hook_value(value, entry, workspace);
    let value = |name: &str| placeholder_value(name, entry, workspace);
    if !entry.args.is_empty() {
        // A program that is itself a shell, also behind `env NAME=value`,
        // reads its script argument as shell text, which gets the shell
        // form's treatment; any other argument, and any other program's,
        // gets the value filled in.
        let program = expand(&entry.command);
        let script = shell_script(&program, &entry.args);
        let arguments = entry
            .args
            .iter()
            .enumerate()
            .map(|(index, arg)| match script.filter(|(at, _)| *at == index) {
                Some((_, ShellKind::Posix)) => quoted_placeholders(ShellKind::Posix, arg, &value),
                Some((_, ShellKind::PowerShell)) => powershell_command(arg, &value),
                None => expand(arg),
            })
            .collect();
        return (program, arguments);
    }
    let shell = entry.shell.as_deref().unwrap_or("sh");
    let arguments = if shell_kind(shell) == Some(ShellKind::PowerShell) {
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            powershell_command(&entry.command, &value),
        ]
    } else {
        vec![
            "-lc".to_owned(),
            quoted_placeholders(ShellKind::Posix, &entry.command, &value),
        ]
    };
    (shell.to_owned(), arguments)
}

fn shell_kind(program: &str) -> Option<ShellKind> {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    match name.strip_suffix(".exe").unwrap_or(&name) {
        "sh" | "bash" | "zsh" | "dash" | "ksh" => Some(ShellKind::Posix),
        "powershell" | "pwsh" => Some(ShellKind::PowerShell),
        _ => None,
    }
}

/// The argument of `args` a shell reads as its script, and which shell: the
/// program itself, or the first word after an `env` program's `NAME=value`
/// assignments and options. Windows PowerShell 5's bare positional command
/// (no `-Command`) is not recognised and keeps the values filled in.
fn shell_script(program: &str, args: &[String]) -> Option<(usize, ShellKind)> {
    let is_env = program.rsplit(['/', '\\']).next().is_some_and(|name| {
        name.eq_ignore_ascii_case("env") || name.eq_ignore_ascii_case("env.exe")
    });
    let (offset, kind) = if is_env {
        let at = args
            .iter()
            .position(|arg| !arg.contains('=') && !arg.starts_with('-'))?;
        (at + 1, shell_kind(&args[at])?)
    } else {
        (0, shell_kind(program)?)
    };
    let rest = &args[offset..];
    let flag = rest.iter().position(|arg| match kind {
        ShellKind::Posix => {
            arg.starts_with('-')
                && !arg.starts_with("--")
                && arg[1..].chars().all(|c| c.is_ascii_alphabetic())
                && arg.contains('c')
        }
        ShellKind::PowerShell => powershell_command_flag(arg),
    })?;
    (flag + 1 < rest.len()).then_some((offset + flag + 1, kind))
}

/// Whether `arg` is PowerShell's `-Command` in any of the spellings it
/// accepts: `-c`, any prefix of `-Command` from `-co`, `/Command` and its
/// prefixes, and `-CommandWithArgs` (`-cwa`).
fn powershell_command_flag(arg: &str) -> bool {
    let Some(name) = arg.strip_prefix('-').or_else(|| arg.strip_prefix('/')) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    !name.is_empty() && ("command".starts_with(&name) || name == "commandwithargs" || name == "cwa")
}

/// The placeholders a hook may use, all set in its environment.
const HOOK_PLACEHOLDERS: [&str; 8] = [
    "HAARNESS_PROJECT_DIR",
    "OPENCODE_PROJECT_DIR",
    "CLAUDE_PROJECT_DIR",
    "HAARNESS_PLUGIN_ROOT",
    "OPENCODE_PLUGIN_ROOT",
    "CLAUDE_PLUGIN_ROOT",
    "HAARNESS_PLUGIN_DATA",
    "CLAUDE_PLUGIN_DATA",
];

/// A PowerShell command whose placeholders PowerShell itself resolves, so a
/// folder named `a$(calc)` never runs `calc`. Each statement that starts
/// with a placeholder (at the start, after `;`, `|`, `&&`, `||` or a line
/// break) is a path to run, so that word is quoted and called with `&`.
fn powershell_command(command: &str, value: &dyn Fn(&str) -> String) -> String {
    quoted_placeholders(ShellKind::PowerShell, command, value)
}

/// The length of the hook placeholder `rest` starts with, if any.
fn placeholder_at(rest: &str) -> Option<usize> {
    HOOK_PLACEHOLDERS
        .iter()
        .find(|name| rest.starts_with(&format!("${{{name}}}")))
        .map(|name| name.len() + 3)
}

/// `command` with each placeholder left for the shell to expand from the
/// environment the hook gets (see [`place_placeholders`]).
fn quoted_placeholders(kind: ShellKind, command: &str, value: &dyn Fn(&str) -> String) -> String {
    place_placeholders(kind, command, &placeholder_at, value)
}

/// What `${name}` stands for in a hook of `entry` run for `workspace`.
fn placeholder_value(name: &str, entry: &HookEntry, workspace: &std::path::Path) -> String {
    let root = || {
        entry
            .plugin_root
            .as_deref()
            .map_or_else(String::new, |root| root.display().to_string())
    };
    match name {
        "HAARNESS_PROJECT_DIR" | "OPENCODE_PROJECT_DIR" | "CLAUDE_PROJECT_DIR" => {
            workspace.display().to_string()
        }
        "HAARNESS_PLUGIN_ROOT" | "OPENCODE_PLUGIN_ROOT" | "CLAUDE_PLUGIN_ROOT" => root(),
        "HAARNESS_PLUGIN_DATA" | "CLAUDE_PLUGIN_DATA" => entry
            .plugin_root
            .as_deref()
            .map_or_else(String::new, |root| root.join(".data").display().to_string()),
        _ => String::new(),
    }
}

fn expand_hook_value(value: &str, entry: &HookEntry, workspace: &std::path::Path) -> String {
    HOOK_PLACEHOLDERS
        .iter()
        .fold(value.to_owned(), |text, name| {
            text.replace(
                &format!("${{{name}}}"),
                &placeholder_value(name, entry, workspace),
            )
        })
}

#[derive(Default)]
struct HookOutcome {
    block: Option<String>,
    context: Option<String>,
    permission: Option<PermissionDecision>,
    updated_input: Option<Value>,
    always: bool,
}

impl From<HookOutcome> for HookDecision {
    fn from(outcome: HookOutcome) -> Self {
        Self {
            block: outcome.block,
            context: outcome.context,
            permission: outcome.permission,
            updated_input: outcome.updated_input,
            always: outcome.always,
        }
    }
}

/// Whether a `PermissionRequest` allow answers Always: `"always": true`, or
/// Claude Code's `updatedPermissions` with a rule that allows. Haarness
/// remembers what its own Always remembers, never the rules or destination
/// the hook names, so a hook cannot widen what it was asked about.
fn answers_always(decision: &Value) -> bool {
    decision.get("always").and_then(Value::as_bool) == Some(true)
        || decision
            .get("updatedPermissions")
            .and_then(Value::as_array)
            .is_some_and(|updates| {
                updates
                    .iter()
                    .any(|update| match update.get("type").and_then(Value::as_str) {
                        Some("addRules" | "replaceRules") => {
                            update.get("behavior").and_then(Value::as_str) == Some("allow")
                        }
                        Some("toolAlwaysAllow") => true,
                        _ => false,
                    })
            })
}

/// Longest program name, failure stderr and block reason a log line carries.
const PROGRAM_CHARS: usize = 120;
const STDERR_CHARS: usize = 300;
const REASON_CHARS: usize = 200;
/// Hook text considered before redaction; the rest is never read.
const SCAN_CHARS: usize = 2000;

/// What a hook's log lines may name: its event, the tool it ran for and the
/// program its command starts, never the configured command line.
struct HookLabel {
    event: String,
    /// The chat the hook ran for, so a slow or failing hook can be traced to
    /// the session that started it.
    session: Option<String>,
    tool: Option<String>,
    program: String,
}

impl HookLabel {
    fn new(entry: &HookEntry, payload: &Value) -> Self {
        Self {
            event: entry.event.clone(),
            session: payload
                .get("session_id")
                .and_then(Value::as_str)
                .map(|session| log_text(session, PROGRAM_CHARS)),
            tool: payload
                .get("tool_name")
                .and_then(Value::as_str)
                .map(|tool| log_text(tool, PROGRAM_CHARS)),
            program: program_word(entry),
        }
    }

    /// One WARN per failed run: a non-zero exit other than a clean block, or
    /// a signal, with the start of stderr redacted and clipped.
    fn failed(&self, status: std::process::ExitStatus, stderr: &[u8], asynchronous: bool) {
        let stderr = log_text(
            head(&String::from_utf8_lossy(stderr), SCAN_CHARS),
            STDERR_CHARS,
        );
        tracing::warn!(
            event = %self.event,
            session = self.session.as_deref(),
            tool = self.tool.as_deref(),
            program = %self.program,
            code = status.code(),
            signal = exit_signal(status),
            asynchronous = asynchronous.then_some(true),
            stderr = (!stderr.is_empty()).then_some(stderr.as_str()),
            "hook command failed"
        );
    }

    fn wait_failed(&self, error: &std::io::Error, asynchronous: bool) {
        tracing::warn!(
            event = %self.event,
            session = self.session.as_deref(),
            tool = self.tool.as_deref(),
            program = %self.program,
            asynchronous = asynchronous.then_some(true),
            %error,
            "hook failed"
        );
    }

    fn timed_out(&self, timeout: Duration, asynchronous: bool) {
        tracing::warn!(
            event = %self.event,
            session = self.session.as_deref(),
            tool = self.tool.as_deref(),
            program = %self.program,
            timeout_secs = timeout.as_secs(),
            asynchronous = asynchronous.then_some(true),
            "hook timed out"
        );
    }

    /// One INFO per block, with the first line of its reason. Events whose
    /// block the runtime ignores log at DEBUG so the INFO line stays true.
    fn blocked(&self, code: Option<i32>, reason: &str) {
        let redacted = redact(head(reason, SCAN_CHARS));
        let line = redacted
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        let reason = log_text(line, REASON_CHARS);
        if matches!(
            self.event.as_str(),
            "PreToolUse" | "PermissionRequest" | "UserPromptSubmit"
        ) {
            tracing::info!(
                event = %self.event,
                session = self.session.as_deref(),
                tool = self.tool.as_deref(),
                program = %self.program,
                code,
                reason = %reason,
                "hook blocked"
            );
        } else {
            tracing::debug!(
                event = %self.event,
                session = self.session.as_deref(),
                program = %self.program,
                code,
                reason = %reason,
                "hook block ignored: this event cannot be blocked"
            );
        }
    }
}

fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// A word of the leading simple command, unquoted as the shell would.
struct ShellWord {
    text: String,
    /// Starts with an unquoted `NAME=`.
    assignment: bool,
}

/// Characters that mean a program word still needs the shell to resolve it.
const SHELL_SPECIAL: &[char] = &[
    '$', '`', '{', '}', '(', ')', '*', '?', '[', ']', '=', ';', '&', '|', '<', '>', '\'', '"', '#',
    '!', '~', '%',
];

/// The program a hook command starts, for diagnostics only: the file name of
/// its first literal command word after leading `NAME=value` assignments and
/// `env`/`exec`/`command`/`nohup` wrappers. Arguments and assignment values
/// never appear. A line only the shell itself could split (command
/// substitution, unbalanced quotes, an expansion in the program word) names
/// the shell instead.
fn program_word(entry: &HookEntry) -> String {
    let shell = entry.shell.as_deref().unwrap_or("sh");
    let fallback = || log_text(file_name(shell).unwrap_or("sh"), PROGRAM_CHARS);
    let literal = |text: &str| ShellWord {
        text: text.to_owned(),
        assignment: false,
    };
    let words: Vec<ShellWord> = if !entry.args.is_empty() {
        std::iter::once(&entry.command)
            .chain(&entry.args)
            .map(|word| literal(word))
            .collect()
    } else if shell.eq_ignore_ascii_case("powershell") {
        // PowerShell quotes differently; only a bare leading path is named.
        entry
            .command
            .split_whitespace()
            .next()
            .map(literal)
            .into_iter()
            .collect()
    } else {
        match shell_words(&entry.command) {
            Some(words) => words,
            None => return fallback(),
        }
    };
    unwrap_program(&words)
        .and_then(file_name)
        .filter(|name| !name.contains(SHELL_SPECIAL))
        .map_or_else(fallback, |name| log_text(name, PROGRAM_CHARS))
}

fn file_name(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\']).find(|part| !part.is_empty())
}

/// Skips leading assignments and transparent wrappers with their options.
/// A wrapper with nothing after it is itself the program.
fn unwrap_program(words: &[ShellWord]) -> Option<&str> {
    let mut words = words.iter().skip_while(|word| word.assignment).peekable();
    loop {
        let wrapper = words.next()?;
        let name = file_name(&wrapper.text)?;
        let takes_value: &[&str] = match name {
            "env" => &["-u", "-C", "-P", "--unset", "--chdir"],
            "exec" => &["-a"],
            "command" | "nohup" => &[],
            _ => return Some(&wrapper.text),
        };
        let mut options = true;
        while let Some(word) = words.peek() {
            let text = word.text.as_str();
            if name == "env" && (text.starts_with("-S") || text.starts_with("--split-string")) {
                // `env -S` splits its own string; only a shell parser could follow it.
                return Some(&wrapper.text);
            }
            if options && text == "--" {
                options = false;
            } else if options && text.starts_with('-') {
                if takes_value.contains(&text) {
                    words.next();
                }
            } else if !(name == "env" && text.contains('=')) {
                break;
            }
            words.next();
        }
        if words.peek().is_none() {
            return Some(&wrapper.text);
        }
    }
}

/// Words of the leading simple command of a POSIX shell line, up to its first
/// operator, with quotes and escapes removed. `None` when the line needs real
/// shell evaluation first: command substitution or unbalanced quoting.
fn shell_words(line: &str) -> Option<Vec<ShellWord>> {
    let is_name = |text: &str| {
        text.chars()
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && text
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
    };
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    while words.len() < 32 {
        while chars
            .next_if(|character| matches!(character, ' ' | '\t'))
            .is_some()
        {}
        if chars
            .peek()
            .is_none_or(|character| "\n;&|<>()#".contains(*character))
        {
            break;
        }
        let mut text = String::new();
        // Decided by the first character that is not part of a leading `NAME`.
        let mut assignment = None;
        while let Some(&character) = chars.peek() {
            if character.is_whitespace() || "\n;&|<>()".contains(character) {
                break;
            }
            chars.next();
            match character {
                '\'' => {
                    assignment.get_or_insert(false);
                    loop {
                        match chars.next()? {
                            '\'' => break,
                            quoted => text.push(quoted),
                        }
                    }
                }
                '"' => {
                    assignment.get_or_insert(false);
                    loop {
                        match chars.next()? {
                            '"' => break,
                            '`' => return None,
                            '$' if chars.peek() == Some(&'(') => return None,
                            '\\' => {
                                let escaped = chars.next()?;
                                if !"$`\"\\\n".contains(escaped) {
                                    text.push('\\');
                                }
                                if escaped != '\n' {
                                    text.push(escaped);
                                }
                            }
                            quoted => text.push(quoted),
                        }
                    }
                }
                '\\' => {
                    assignment.get_or_insert(false);
                    let escaped = chars.next()?;
                    if escaped != '\n' {
                        text.push(escaped);
                    }
                }
                '`' => return None,
                '$' if chars.peek() == Some(&'(') => return None,
                '=' if assignment.is_none() && is_name(&text) => {
                    assignment = Some(true);
                    text.push(character);
                }
                other => {
                    if assignment.is_none() && !(other == '_' || other.is_ascii_alphanumeric()) {
                        assignment = Some(false);
                    }
                    text.push(other);
                }
            }
        }
        words.push(ShellWord {
            text,
            assignment: assignment == Some(true),
        });
    }
    Some(words)
}

/// At most `max` characters from the start of `text`, cut back to whitespace
/// so a longer secret never survives as a fragment that redaction misses.
fn head(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        None => text,
        Some((end, next)) if next.is_whitespace() => &text[..end],
        Some((end, _)) => text[..end]
            .rfind(char::is_whitespace)
            .map_or("", |at| &text[..at]),
    }
}

/// The free-text scrubbers live in `haarness_core::redact`; these names stay
/// for the server's own log lines.
pub(crate) use haarness_core::redact::{log_text, text as redact};

/// The tools whose successful completion runs `FileChanged`.
const FILE_TOOLS: &[&str] = &[
    "write",
    "edit",
    "multiedit",
    "notebook_edit",
    "notebookedit",
];

/// Native tools whose successful completion changes workspace files, with
/// the argument that names the file. Shell commands are not inspected.
fn changed_file(tool: &str, arguments: &Value) -> Option<String> {
    if !FILE_TOOLS.contains(&tool) {
        return None;
    }
    arguments
        .get("path")
        .or_else(|| arguments.get("file_path"))
        .or_else(|| arguments.get("notebook_path"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The `StopFailure` categories of provider errors. Before they were told
/// apart every provider error was `provider`, which still matches them all.
const PROVIDER_FAILURES: &[&str] = &[
    "authentication_failed",
    "rate_limit",
    "overloaded",
    "server_error",
    "invalid_request",
];

/// What a hook's matcher is compared with for an event: what an editor
/// suggests, and what decides whether a matcher can ever match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherTarget {
    /// The tool's name (Claude aliases accepted).
    Tool,
    /// The permission asked (`bash`, `edit`, `external_directory`, …).
    Permission,
    /// One of these values, the only ones Haarness sends for the event.
    Values(&'static [&'static str]),
    /// Nothing: the matcher is ignored and the hook runs every time.
    Nothing,
}

/// See [`MatcherTarget`].
#[must_use]
pub fn matcher_target(event: &str) -> MatcherTarget {
    match event {
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => MatcherTarget::Tool,
        "PermissionRequest" => MatcherTarget::Permission,
        "SessionStart" => MatcherTarget::Values(&["startup"]),
        "SessionEnd" => MatcherTarget::Values(&["archived"]),
        "Notification" => MatcherTarget::Values(&["permission_prompt", "turn_completed"]),
        "PostCompact" => MatcherTarget::Values(&["manual", "automatic"]),
        "StopFailure" => MatcherTarget::Values(&[
            "authentication_failed",
            "rate_limit",
            "overloaded",
            "server_error",
            "invalid_request",
            "provider",
            "incomplete",
            "content_filter",
            "tool",
            "other",
        ]),
        "FileChanged" => MatcherTarget::Values(FILE_TOOLS),
        _ => MatcherTarget::Nothing,
    }
}

/// Whether a hook for `event` with `matcher` can never run: the event sends
/// only certain values, and the matcher matches none of them.
#[must_use]
pub fn never_runs(event: &str, matcher: &str) -> bool {
    let matcher = matcher.trim();
    if matcher.is_empty() || matcher == "*" {
        return false;
    }
    match matcher_target(event) {
        MatcherTarget::Values(values) => {
            !values.iter().any(|value| matcher_matches(matcher, value))
        }
        _ => false,
    }
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
        patterns: &[String],
        always: Option<&[String]>,
        detail: &Value,
    ) -> HookDecision {
        let mut payload = json!({
            "hook_event_name": "PermissionRequest",
            "session_id": thread,
            "cwd": self.workspace,
            "tool_name": permission,
            "tool_input": detail,
            "permission_patterns": patterns,
            "allow_always": always.is_some(),
        });
        // What Always would remember, as Haarness's own patterns and as the
        // `permission_suggestions` a Claude Code hook echoes back in
        // `updatedPermissions`.
        if let Some(always) = always {
            payload["always_patterns"] = json!(always);
            payload["permission_suggestions"] = json!([{
                "type": "addRules",
                "rules": always
                    .iter()
                    .map(|pattern| json!({"toolName": permission, "ruleContent": pattern}))
                    .collect::<Vec<_>>(),
                "behavior": "allow",
                "destination": "session",
            }]);
        }
        self.run_event("PermissionRequest", Some(permission), &payload)
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

/// Records log events for tests that assert what a line does and does not say.
#[cfg(test)]
pub(crate) mod log_capture {
    use std::fmt::Write as _;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// One rendered line per event: `LEVEL message key=value …`.
    #[derive(Clone, Default)]
    pub(crate) struct Capture(Arc<Mutex<Vec<String>>>);

    /// With a single registered dispatcher, tracing caches a callsite's
    /// interest and the max level from whichever thread hits it first, so a
    /// parallel test thread without a subscriber would silence this capture.
    /// A second, permanently registered dispatcher keeps the registry
    /// consulting every live capture.
    static KEEPALIVE: std::sync::LazyLock<tracing::Dispatch> =
        std::sync::LazyLock::new(|| tracing::Dispatch::new(Capture::default()));

    impl Capture {
        /// Records this thread's events until the guard drops. The tokio test
        /// runtime is single-threaded, so spawned tasks are recorded too.
        pub(crate) fn install() -> (Self, tracing::subscriber::DefaultGuard) {
            std::sync::LazyLock::force(&KEEPALIVE);
            let capture = Self::default();
            let guard = tracing::subscriber::set_default(capture.clone());
            (capture, guard)
        }

        pub(crate) fn lines(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }

        /// Lines whose message is exactly `message`.
        pub(crate) fn matching(&self, message: &str) -> Vec<String> {
            self.lines()
                .into_iter()
                .filter(|line| {
                    line.split_once(' ')
                        .is_some_and(|(_, rest)| rest.split(" |").next() == Some(message))
                })
                .collect()
        }
    }

    #[derive(Default)]
    struct Line {
        message: String,
        fields: String,
    }

    impl Visit for Line {
        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.message = value.to_owned();
            } else {
                let _ = write!(self.fields, " {}={value}", field.name());
            }
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.record_str(field, &format!("{value:?}"));
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut line = Line::default();
            event.record(&mut line);
            self.0.lock().unwrap().push(format!(
                "{} {} |{}",
                event.metadata().level(),
                line.message,
                line.fields
            ));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log_capture::Capture;

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
    fn a_matcher_that_can_never_match_its_event_is_told_apart() {
        assert!(!never_runs("SessionEnd", "archived"));
        assert!(!never_runs("SessionEnd", "*"));
        assert!(!never_runs("SessionEnd", " "));
        assert!(never_runs("SessionEnd", "clear"));
        assert!(never_runs("SessionStart", "resume"));
        assert!(!never_runs("PostCompact", "manual|automatic"));
        assert!(never_runs("PostCompact", "auto"));
        assert!(!never_runs("PostCompact", "auto.*"));
        assert!(never_runs("FileChanged", "Bash"));
        assert!(!never_runs("FileChanged", "Edit"));
        assert!(!never_runs("FileChanged", "notebookedit"));
        assert!(!never_runs("StopFailure", "rate_limit|provider"));
        assert!(never_runs("StopFailure", "billing_error"));
        // Tools and permissions are open-ended, and an event with nothing
        // to match ignores its matcher.
        assert!(!never_runs("PreToolUse", "mcp__github__create_issue"));
        assert!(!never_runs("PermissionRequest", "bash"));
        assert!(!never_runs("Stop", "bash"));
        assert_eq!(matcher_target("UserPromptSubmit"), MatcherTarget::Nothing);
    }

    #[tokio::test]
    async fn a_stop_failure_matcher_for_provider_still_matches_every_provider_error() {
        let thread = ThreadId::new();
        let turn = haarness_protocol::TurnId::new();
        let events = captured(
            json!({"StopFailure": [{"matcher": "provider"}, {"matcher": "rate_limit"}]}),
            async |hooks| {
                for error in ["rate_limit", "authentication_failed", "provider", "tool"] {
                    hooks
                        .stop_failure(thread, turn, &json!({"error": error}))
                        .await;
                }
            },
        )
        .await;
        let errors: Vec<&str> = events
            .iter()
            .map(|event| event["error"].as_str().unwrap())
            .collect();
        assert_eq!(
            errors,
            [
                "rate_limit",
                "rate_limit",
                "authentication_failed",
                "provider"
            ]
        );
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

    #[test]
    fn powershell_hooks_read_placeholders_from_the_environment() {
        let entry = |command: &str, args: &[&str], shell: Option<&str>, root: &str| HookEntry {
            event: "Stop".into(),
            matcher: None,
            command: command.into(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            shell: shell.map(str::to_owned),
            timeout: Duration::from_secs(1),
            plugin_root: Some(PathBuf::from(root)),
            asynchronous: false,
        };
        let folder = std::path::Path::new(r"C:\src\a$(calc)");
        let script = |command: &str, shell: &str, root: &str| {
            hook_invocation(&entry(command, &[], Some(shell), root), folder)
                .1
                .last()
                .cloned()
                .unwrap_or_default()
        };
        // Double quotes and bare words read the environment. Inside single
        // quotes a plain value goes in as it is; any other is never pasted.
        assert_eq!(
            script(
                r#"Get-Item "${CLAUDE_PROJECT_DIR}\x"; & '${CLAUDE_PLUGIN_ROOT}\gate.ps1' '${CLAUDE_PROJECT_DIR}' ${HOME}"#,
                "powershell",
                r"C:\plugins\kit",
            ),
            r#"Get-Item "${env:CLAUDE_PROJECT_DIR}\x"; & 'C:\plugins\kit\gate.ps1' '${env:CLAUDE_PROJECT_DIR}' ${HOME}"#
        );
        for root in [
            r"C:\plugins\it's",
            "C:\\plugins\\it\u{2019}s",
            r"C:\a$(calc)",
        ] {
            let command = script("& '${CLAUDE_PLUGIN_ROOT}\\gate.ps1'", "powershell", root);
            assert_eq!(command, r"& '${env:CLAUDE_PLUGIN_ROOT}\gate.ps1'", "{root}");
        }
        // A curly quote closes a PowerShell single-quoted string too.
        assert_eq!(
            script(
                "Write-Output \u{2018}${CLAUDE_PROJECT_DIR}\u{2019}",
                "pwsh",
                r"C:\p"
            ),
            "Write-Output \u{2018}${env:CLAUDE_PROJECT_DIR}\u{2019}"
        );
        // A quote inside double quotes, or escaped, does not open a string.
        assert_eq!(
            script(
                r#"Write-Output "it's ${CLAUDE_PROJECT_DIR}" `'${CLAUDE_PROJECT_DIR}"#,
                "powershell",
                r"C:\p"
            ),
            r#"Write-Output "it's ${env:CLAUDE_PROJECT_DIR}" `'${env:CLAUDE_PROJECT_DIR}"#
        );
        // Every statement that starts with a placeholder is a path to call.
        assert_eq!(
            script(
                "${CLAUDE_PLUGIN_ROOT}\\a.ps1 -Strict; Write-Output x | ${CLAUDE_PROJECT_DIR}\\b.ps1\n${CLAUDE_PLUGIN_ROOT}\\c.ps1 && ${CLAUDE_PLUGIN_ROOT}\\d.ps1 || & ${CLAUDE_PLUGIN_ROOT}\\e.ps1; . ${CLAUDE_PLUGIN_ROOT}\\f.ps1",
                "pwsh",
                r"C:\p",
            ),
            "& \"${env:CLAUDE_PLUGIN_ROOT}\\a.ps1\" -Strict; Write-Output x | & \"${env:CLAUDE_PROJECT_DIR}\\b.ps1\"\n& \"${env:CLAUDE_PLUGIN_ROOT}\\c.ps1\" && & \"${env:CLAUDE_PLUGIN_ROOT}\\d.ps1\" || & \"${env:CLAUDE_PLUGIN_ROOT}\\e.ps1\"; . \"${env:CLAUDE_PLUGIN_ROOT}\\f.ps1\""
        );
        // An argument list whose program is a shell, also behind `env`, in
        // any spelling of -Command: its script argument is shell text;
        // other arguments, and other programs, get the value.
        for flag in [
            "-Command",
            "-c",
            "-com",
            "-COMM",
            "/Command",
            "/c",
            "-CommandWithArgs",
            "-cwa",
        ] {
            let (program, arguments) = hook_invocation(
                &entry(
                    "pwsh.exe",
                    &["-NoProfile", flag, "& \"${CLAUDE_PROJECT_DIR}\\x.ps1\""],
                    None,
                    r"C:\p",
                ),
                folder,
            );
            assert_eq!(program, "pwsh.exe");
            assert_eq!(
                arguments[2], "& \"${env:CLAUDE_PROJECT_DIR}\\x.ps1\"",
                "{flag}"
            );
        }
        let (_, arguments) = hook_invocation(
            &entry(
                "/usr/bin/env",
                &[
                    "LANG=C",
                    "-i",
                    "bash",
                    "-c",
                    "cat '${CLAUDE_PROJECT_DIR}/x' \"$1\"",
                    "sh",
                    "${CLAUDE_PROJECT_DIR}",
                ],
                None,
                r"C:\p",
            ),
            folder,
        );
        assert_eq!(arguments[4], "cat ''\"${CLAUDE_PROJECT_DIR}\"'/x' \"$1\"");
        assert_eq!(arguments[6], r"C:\src\a$(calc)");
        assert_eq!(
            hook_invocation(
                &entry("gate.exe", &["${CLAUDE_PROJECT_DIR}"], None, r"C:\p"),
                folder
            )
            .1,
            [r"C:\src\a$(calc)"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_hooks_never_read_the_project_folder_name_as_shell_syntax() {
        let root =
            std::env::temp_dir().join(format!("haarness-hook-folder-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("it's $(touch INJECTED) \"here\"");
        std::fs::create_dir_all(&workspace).unwrap();
        let marker = root.join("hook.txt");
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PostToolUseFailure": [{"hooks": [{
                    "command": format!(
                        "printf '%s|%s|%s|%s' \"${{CLAUDE_PROJECT_DIR}}\" \"${{OPENCODE_PROJECT_DIR}}\" \"$HAARNESS_PROJECT_DIR\" '${{CLAUDE_PROJECT_DIR}}' > '{}'",
                        marker.display()
                    )
                }]}]
            })),
            workspace.clone(),
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
        let rendered = workspace.display().to_string();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!("{rendered}|{rendered}|{rendered}|{rendered}")
        );
        assert!(!workspace.join("INJECTED").exists());
        assert!(!root.join("INJECTED").exists());
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
            .permission_request(
                ThreadId::new(),
                "bash",
                &["cargo test".to_owned()],
                None,
                &json!({"command": "cargo test"}),
            )
            .await;
        assert_eq!(decision.permission, Some(PermissionDecision::Allow));
        assert!(decision.block.is_none());
    }

    /// Asks a `PermissionRequest` hook that prints `answer` about
    /// `git status --short`, offering `always`; returns its decision and the
    /// input it was given.
    async fn permission_answer(answer: Value, always: Option<&[String]>) -> (HookDecision, Value) {
        let root =
            std::env::temp_dir().join(format!("haarness-permission-hook-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let input = root.join("input.json");
        let reply = root.join("reply.json");
        std::fs::write(&reply, answer.to_string()).unwrap();
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PermissionRequest": [{"matcher": "bash", "hooks": [{
                    "command": format!("cat > '{}'; cat '{}'", input.display(), reply.display())
                }]}]
            })),
            root.clone(),
        );
        let decision = hooks
            .permission_request(
                ThreadId::new(),
                "bash",
                &["git status --short".to_owned()],
                always,
                &json!({"command": "git status --short"}),
            )
            .await;
        let seen = serde_json::from_str(&std::fs::read_to_string(&input).unwrap()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        (decision, seen)
    }

    fn permission_reply(decision: Value) -> Value {
        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}})
    }

    #[tokio::test]
    async fn a_permission_request_hook_can_answer_always() {
        let always = ["git status *".to_owned()];
        let (decision, seen) = permission_answer(
            permission_reply(json!({"behavior": "allow", "always": true})),
            Some(&always),
        )
        .await;
        assert_eq!(decision.permission, Some(PermissionDecision::Allow));
        assert!(decision.always);
        // The hook is told it may answer Always and what that remembers, in
        // Haarness's words and as Claude Code's suggestions.
        assert_eq!(seen["allow_always"], true);
        assert_eq!(seen["always_patterns"], json!(["git status *"]));
        assert_eq!(
            seen["permission_suggestions"],
            json!([{"type": "addRules", "rules": [{"toolName": "bash", "ruleContent": "git status *"}],
                "behavior": "allow", "destination": "session"}])
        );
        // Claude Code's shape: the suggestions handed back.
        let (decision, _) = permission_answer(
            permission_reply(
                json!({"behavior": "allow", "updatedPermissions": seen["permission_suggestions"]}),
            ),
            Some(&always),
        )
        .await;
        assert!(decision.always);
        // A plain allow, or an update that is not an allow rule, is once.
        for answer in [
            json!({"behavior": "allow"}),
            json!({"behavior": "allow", "always": false}),
            json!({"behavior": "allow", "updatedPermissions": [{"type": "setMode", "mode": "acceptEdits", "destination": "session"}]}),
            json!({"behavior": "allow", "updatedPermissions": [{"type": "addRules", "rules": [], "behavior": "deny", "destination": "session"}]}),
        ] {
            let (decision, _) =
                permission_answer(permission_reply(answer.clone()), Some(&always)).await;
            assert_eq!(
                decision.permission,
                Some(PermissionDecision::Allow),
                "{answer}"
            );
            assert!(!decision.always, "{answer}");
        }
    }

    #[tokio::test]
    async fn a_request_that_offers_no_always_says_so_to_the_hook() {
        let (decision, seen) = permission_answer(
            permission_reply(json!({"behavior": "allow", "always": true})),
            None,
        )
        .await;
        assert_eq!(seen["allow_always"], false);
        assert!(seen.get("always_patterns").is_none());
        assert!(seen.get("permission_suggestions").is_none());
        // The runtime ignores `always` here; the hook still allowed.
        assert_eq!(decision.permission, Some(PermissionDecision::Allow));
    }

    #[tokio::test]
    async fn always_needs_every_allowing_hook_and_a_successful_one() {
        let run = |commands: Vec<&str>| {
            let hooks = ConfiguredHooks::new(
                ConfiguredHooks::parse_block(&json!({
                    "PermissionRequest": [{"matcher": "bash", "hooks": commands
                        .iter()
                        .map(|command| json!({"command": command}))
                        .collect::<Vec<_>>()}]
                })),
                std::env::temp_dir(),
            );
            async move {
                hooks
                    .permission_request(
                        ThreadId::new(),
                        "bash",
                        &["git status".to_owned()],
                        Some(&["git status *".to_owned()]),
                        &json!({"command": "git status"}),
                    )
                    .await
            }
        };
        let always =
            r#"echo '{"hookSpecificOutput":{"decision":{"behavior":"allow","always":true}}}'"#;
        let once = r#"echo '{"hookSpecificOutput":{"decision":{"behavior":"allow"}}}'"#;
        assert!(run(vec![always, always]).await.always);
        assert!(run(vec![always, "true"]).await.always);
        let mixed = run(vec![always, once]).await;
        assert_eq!(mixed.permission, Some(PermissionDecision::Allow));
        assert!(!mixed.always);
        let failed = run(vec![&format!("{always}; exit 1")]).await;
        assert!(failed.permission.is_none() && !failed.always);
    }

    #[tokio::test]
    async fn a_permission_request_allow_says_when_it_rewrote_the_input() {
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PermissionRequest": [{"matcher": "bash", "hooks": [{
                    "command": r#"echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedInput":{"command":"npm ci"}}}}'"#
                }]}]
            })),
            std::env::temp_dir(),
        );
        let decision = hooks
            .permission_request(
                ThreadId::new(),
                "bash",
                &["npm install".to_owned()],
                None,
                &json!({"command": "npm install"}),
            )
            .await;
        assert_eq!(decision.permission, Some(PermissionDecision::Allow));
        assert_eq!(decision.updated_input, Some(json!({"command": "npm ci"})));
    }

    #[tokio::test]
    async fn a_failed_hook_cannot_approve_or_rewrite_but_can_still_refuse() {
        let run = |command: &str| {
            let hooks = ConfiguredHooks::new(
                ConfiguredHooks::parse_block(&json!({
                    "PermissionRequest": [{"matcher": "bash", "hooks": [{"command": command}]}],
                    "PreToolUse": [{"matcher": "Bash", "hooks": [{"command": command}]}]
                })),
                std::env::temp_dir(),
            );
            async move {
                let permission = hooks
                    .permission_request(
                        ThreadId::new(),
                        "bash",
                        &["rm -rf build".to_owned()],
                        None,
                        &json!({"command": "rm -rf build"}),
                    )
                    .await;
                let pre = hooks
                    .pre_tool_use(ThreadId::new(), "bash", &json!({"command": "rm -rf build"}))
                    .await;
                (permission, pre)
            }
        };
        let (permission, pre) = run(r#"echo '{"hookSpecificOutput":{"permissionDecision":"allow","decision":{"behavior":"allow"},"updatedInput":{"command":"true"}}}'; exit 1"#).await;
        assert_eq!(permission.permission, None);
        assert_eq!(pre.updated_input, None);
        assert!(permission.block.is_none() && pre.block.is_none());
        let (permission, _) = run(r#"echo '{"hookSpecificOutput":{"decision":{"behavior":"deny","message":"no"}}}'; exit 1"#).await;
        assert_eq!(permission.permission, Some(PermissionDecision::Deny));
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
        let root =
            std::env::temp_dir().join(format!("haarness-lifecycle-hooks-{}", uuid::Uuid::now_v7()));
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
            [
                "SessionStart",
                "SessionEnd",
                "TurnEnd",
                "Notification",
                "ConfigChange"
            ]
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
                    .post_tool_use(
                        thread,
                        "write",
                        &json!({"path":"src/lib.rs","content":"x"}),
                        "ok",
                        false,
                    )
                    .await;
                hooks
                    .post_tool_use(
                        thread,
                        "edit",
                        &json!({"path":"README.md","old":"a","new":"b"}),
                        "failed",
                        true,
                    )
                    .await;
                hooks
                    .post_tool_use(
                        thread,
                        "bash",
                        &json!({"command":"touch other"}),
                        "ok",
                        false,
                    )
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

    fn entry(command: &str, args: &[&str], shell: Option<&str>) -> HookEntry {
        HookEntry {
            event: "PreToolUse".into(),
            matcher: None,
            command: command.into(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            shell: shell.map(str::to_owned),
            timeout: Duration::from_secs(5),
            asynchronous: false,
            plugin_root: None,
        }
    }

    #[test]
    fn program_word_never_shows_arguments_or_assignment_values() {
        for (command, args, shell, expected) in [
            ("./check.sh", &[][..], None, "check.sh"),
            ("/usr/local/bin/gate --password hunter2", &[], None, "gate"),
            ("GITHUB_TOKEN=ghp_x gh api /user", &[], None, "gh"),
            (
                "curl -H 'Authorization: Bearer abc' https://x.test",
                &[],
                None,
                "curl",
            ),
            (
                r#"PASSWORD="first PRIVATE_SUFFIX" sh "/tmp/gate.sh""#,
                &[],
                None,
                "sh",
            ),
            (
                "PASSWORD='first PRIVATE_SUFFIX' node gate.js",
                &[],
                None,
                "node",
            ),
            (
                r"PASSWORD=first\ PRIVATE_SUFFIX python3 gate.py",
                &[],
                None,
                "python3",
            ),
            (
                r#"UNRELATED=safe PASSWORD="first PRIVATE_SUFFIX" ruby x.rb"#,
                &[],
                None,
                "ruby",
            ),
            (
                "env -i FOO=bar -u BAZ /usr/bin/python3 hook.py",
                &[],
                None,
                "python3",
            ),
            ("exec -a renamed ./run --token x", &[], None, "run"),
            ("command nohup env -- A=1 deno run x.ts", &[], None, "deno"),
            ("env -S 'node --token=x'", &[], None, "env"),
            ("env", &[], None, "env"),
            (
                r#""$CLAUDE_PLUGIN_ROOT/hooks/fixture gate.sh" arg"#,
                &[],
                None,
                "fixture gate.sh",
            ),
            (
                "${CLAUDE_PLUGIN_ROOT}/hooks/check.sh",
                &[],
                None,
                "check.sh",
            ),
            ("echo ok; rm -rf /", &[], None, "echo"),
            // Only the shell could resolve these; name the shell instead.
            ("$(cat cmd) --token x", &[], None, "sh"),
            ("`which gate` --token x", &[], None, "sh"),
            ("TOKEN=$(cat secret) ./run", &[], None, "sh"),
            (
                r#"PASSWORD="unterminated PRIVATE_SUFFIX ./run"#,
                &[],
                Some("bash"),
                "bash",
            ),
            (
                "TOKEN=${SECRET:-a PRIVATE_SUFFIX} ./run",
                &[],
                Some("/bin/zsh"),
                "zsh",
            ),
            ("$HOOK_BIN --check", &[], None, "sh"),
            ("", &[], None, "sh"),
            // Explicit argument lists run without a shell.
            (
                "${CLAUDE_PLUGIN_ROOT}/bin/check",
                &["--token", "PRIVATE_SUFFIX"],
                None,
                "check",
            ),
            (
                "/usr/bin/env",
                &["API_KEY=PRIVATE_SUFFIX", "node", "x.js"],
                None,
                "node",
            ),
            (
                r"C:\tools\gate.exe -Token PRIVATE_SUFFIX",
                &[],
                Some("powershell"),
                "gate.exe",
            ),
            (
                r#"& "C:\tools\gate.exe""#,
                &[],
                Some("powershell"),
                "powershell",
            ),
        ] {
            let word = program_word(&entry(command, args, shell));
            assert_eq!(word, expected, "{command:?} {args:?}");
            assert!(!word.contains("PRIVATE_SUFFIX"));
        }
        let long = format!("./ga\u{1b}te\u{9b}{}", "é".repeat(300));
        let word = program_word(&entry(&long, &[], None));
        assert!(
            word.chars().all(|character| !character.is_control()),
            "{word:?}"
        );
        assert_eq!(word.chars().count(), PROGRAM_CHARS);
        assert!(word.ends_with('…'));
    }

    #[test]
    fn the_pre_redaction_cut_drops_a_partial_word() {
        assert_eq!(head("one two three", 6), "one");
        assert_eq!(head("one two three", 7), "one two");
        assert_eq!(head("PRIVATESUFFIXPRIVATESUFFIX", 10), "");
        assert_eq!(head("ünï cödé", 100), "ünï cödé");
    }

    fn hooks_for(event: &str, hooks: Value) -> ConfiguredHooks {
        ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({ event: [{"hooks": hooks}] })),
            std::env::temp_dir(),
        )
    }

    fn assert_private(capture: &Capture) {
        for line in capture.lines() {
            assert!(!line.contains("PRIVATE_SUFFIX"), "{line}");
        }
    }

    #[tokio::test]
    async fn failed_hooks_log_one_warning_naming_program_code_and_stderr() {
        let (capture, _guard) = Capture::install();
        let hooks = hooks_for(
            "PreToolUse",
            json!([{"command": "SECRET_TOKEN=PRIVATE_SUFFIX sh -c 'echo \"auth failed: password=PRIVATE_SUFFIX\" >&2; echo second line >&2; exit 1'"}]),
        );
        let decision = hooks
            .pre_tool_use(ThreadId::new(), "bash", &json!({"command": "true"}))
            .await;
        assert!(decision.block.is_none(), "a non-2 failure never blocks");
        let failed = capture.matching("hook command failed");
        assert_eq!(failed.len(), 1, "{:?}", capture.lines());
        assert!(failed[0].starts_with("WARN "), "{}", failed[0]);
        for expected in [
            " event=PreToolUse",
            " tool=bash",
            " program=sh",
            " code=1",
            "stderr=auth failed: password=[redacted] second line",
        ] {
            assert!(failed[0].contains(expected), "{expected}: {}", failed[0]);
        }
        assert!(capture.matching("hook blocked").is_empty());

        hooks_for(
            "UserPromptSubmit",
            json!([{"command": "definitely-missing-hook-binary --token PRIVATE_SUFFIX"}]),
        )
        .user_prompt_submit(ThreadId::new(), "hello")
        .await;
        let missing = capture.matching("hook command failed");
        assert_eq!(missing.len(), 2, "{:?}", capture.lines());
        assert!(missing[1].contains(" program=definitely-missing-hook-binary"));
        assert!(missing[1].contains(" code=127"), "{}", missing[1]);
        assert!(missing[1].contains("not found"), "{}", missing[1]);
        assert!(!missing[1].contains(" tool="));

        #[cfg(unix)]
        {
            hooks_for("Stop", json!([{"command": "kill -9 $$"}]))
                .stop(ThreadId::new())
                .await;
            let killed = capture.matching("hook command failed");
            assert_eq!(killed.len(), 3, "{:?}", capture.lines());
            assert!(killed[2].contains(" signal=9"), "{}", killed[2]);
            assert!(!killed[2].contains(" code="), "{}", killed[2]);
        }

        hooks_for(
            "PreToolUse",
            json!([{"command": "API_KEY=PRIVATE_SUFFIX sleep 5", "timeout": 1}]),
        )
        .pre_tool_use(ThreadId::new(), "bash", &json!({}))
        .await;
        let timed_out = capture.matching("hook timed out");
        assert_eq!(timed_out.len(), 1, "{:?}", capture.lines());
        assert!(timed_out[0].contains(" program=sleep"), "{}", timed_out[0]);
        assert_private(&capture);
    }

    /// A payload larger than the pipe holds, for a hook that never reads it:
    /// writing it first waited for the hook with no limit, and an async hook
    /// held the turn the same way.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hook_that_never_reads_a_large_payload_still_times_out() {
        let (capture, _guard) = Capture::install();
        let input = json!({"command": "x".repeat(1 << 20)});
        let started = std::time::Instant::now();
        let decision =
            tokio::time::timeout(
                Duration::from_secs(10),
                hooks_for("PreToolUse", json!([{"command": "sleep 30", "timeout": 1}]))
                    .pre_tool_use(ThreadId::new(), "bash", &input),
            )
            .await
            .expect("the hook's timeout applies while its payload is written");
        assert!(decision.block.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            capture.matching("hook timed out").len(),
            1,
            "{:?}",
            capture.lines()
        );

        let started = std::time::Instant::now();
        tokio::time::timeout(
            Duration::from_secs(5),
            hooks_for(
                "PreToolUse",
                json!([{"command": "sleep 30", "timeout": 2, "async": true}]),
            )
            .pre_tool_use(ThreadId::new(), "bash", &input),
        )
        .await
        .expect("an async hook never holds the turn");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    /// A hook that prints more than the pipe holds before it reads its
    /// payload waited for its output to be read, while the payload waited for
    /// the hook.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hook_that_writes_much_before_reading_its_payload_finishes() {
        let marker =
            std::env::temp_dir().join(format!("haarness-hook-read-{}", uuid::Uuid::now_v7()));
        let input = json!({"command": "x".repeat(1 << 20)});
        let hooks = hooks_for(
            "PreToolUse",
            json!([{
                "command": format!("head -c 262144 /dev/zero; wc -c > {}", marker.display()),
                "timeout": 20
            }]),
        );
        tokio::time::timeout(
            Duration::from_secs(10),
            hooks.pre_tool_use(ThreadId::new(), "bash", &input),
        )
        .await
        .expect("output is read while the payload is written");
        let read: usize = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(read > 1 << 20, "{read}");
        let _ = std::fs::remove_file(&marker);
    }

    #[tokio::test]
    async fn every_block_logs_once_at_info_with_tool_program_and_reason() {
        let (capture, _guard) = Capture::install();
        let thread = ThreadId::new();
        let exit_two = hooks_for(
            "PreToolUse",
            json!([{"command": "TOKEN=PRIVATE_SUFFIX sh -c 'printf \"policy: no pushes\\nsee token=PRIVATE_SUFFIX\" >&2; exit 2'"}]),
        );
        let decision = exit_two.pre_tool_use(thread, "bash", &json!({})).await;
        assert!(decision.block.is_some());
        let json_deny = hooks_for(
            "PreToolUse",
            json!([{"command": r#"echo '{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"protected path"}}'"#}]),
        );
        let decision = json_deny.pre_tool_use(thread, "edit", &json!({})).await;
        assert_eq!(decision.permission, Some(PermissionDecision::Deny));
        let failed_block = hooks_for(
            "UserPromptSubmit",
            json!([{"command": r#"echo '{"decision":"block","reason":"secret=PRIVATE_SUFFIX in prompt"}'; exit 1"#}]),
        );
        let decision = failed_block.user_prompt_submit(thread, "hi").await;
        assert!(decision.block.is_some());

        let blocked = capture.matching("hook blocked");
        assert_eq!(blocked.len(), 3, "{:?}", capture.lines());
        assert!(blocked.iter().all(|line| line.starts_with("INFO ")));
        // Each line names the chat the hook ran for.
        assert!(
            blocked
                .iter()
                .all(|line| line.contains(&format!(" session={thread}"))),
            "{blocked:?}"
        );
        for (line, expected) in blocked.iter().zip([
            [
                " tool=bash",
                " program=sh",
                " code=2",
                "reason=policy: no pushes",
            ],
            [
                " tool=edit",
                " program=echo",
                " code=0",
                "reason=protected path",
            ],
            [
                " event=UserPromptSubmit",
                " program=echo",
                " code=1",
                "reason=secret=[redacted] in prompt",
            ],
        ]) {
            for expected in expected {
                assert!(line.contains(expected), "{expected}: {line}");
            }
        }
        // Only the block that also exited 1 is a failure; clean blocks are not.
        assert_eq!(capture.matching("hook command failed").len(), 1);

        // Observational events cannot be blocked, so they never claim one at INFO.
        hooks_for("Stop", json!([{"command": "echo stop >&2; exit 2"}]))
            .stop(thread)
            .await;
        assert_eq!(capture.matching("hook blocked").len(), 3);
        assert_eq!(
            capture
                .matching("hook block ignored: this event cannot be blocked")
                .len(),
            1
        );
        assert_private(&capture);
    }

    #[tokio::test]
    async fn async_hook_failures_are_logged_instead_of_dropped() {
        let (capture, _guard) = Capture::install();
        let decision = hooks_for(
            "PreToolUse",
            json!([{"command": "PASSWORD=PRIVATE_SUFFIX sh -c 'echo late failure >&2; exit 3'", "async": true}]),
        )
        .pre_tool_use(ThreadId::new(), "bash", &json!({}))
        .await;
        assert!(decision.block.is_none());
        let failed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let failed = capture.matching("hook command failed");
                if !failed.is_empty() {
                    return failed;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("async failure logged");
        assert_eq!(failed.len(), 1);
        for expected in [
            " asynchronous=true",
            " code=3",
            " program=sh",
            "stderr=late failure",
        ] {
            assert!(failed[0].contains(expected), "{expected}: {}", failed[0]);
        }
        assert_private(&capture);
    }
}
