use super::*;
use haarness_core::{PermissionDecision as D, PermissionRule, RepositoryApprovalStore};
use haarness_protocol::{ItemData, PermissionReply, PermissionReplyDecision as Reply};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-parity-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn rule(pattern: &str, decision: D) -> PermissionRule {
    PermissionRule {
        pattern: pattern.into(),
        decision,
    }
}
fn runtime(
    workspace: &Path,
    config: RuntimeConfig,
    tools: Option<Arc<dyn ToolRegistry>>,
) -> AgentRuntime {
    AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Capturing::default()) as Arc<dyn ModelProvider>,
        ])),
        tools.unwrap_or_else(|| Arc::new(NativeToolRegistry::new(workspace.to_owned()))),
        EventBus::new(2048),
        workspace.to_owned(),
        String::new(),
        config,
    )
}
async fn send(runtime: &AgentRuntime, thread: &Thread, script: &str) -> Turn {
    runtime
        .start_turn(thread.id, turn_request(script))
        .await
        .unwrap()
}
async fn assert_success(runtime: &AgentRuntime, thread: &Thread, turn: &Turn) {
    assert_eq!(wait_done(runtime, turn).await.status, TurnStatus::Completed);
    let items = runtime.items(thread.id, None).await.unwrap();
    let errors: Vec<_> = items
        .iter()
        .filter(|item| matches!(item.data, ItemData::ToolResult { is_error: true, .. }))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[tokio::test]
async fn parity_nested_tasks_with_one_slot_do_not_deadlock() {
    let fixture = Fixture::new();
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.execution.subagent_concurrency = 1;
    config.execution.subagent_depth = 2;
    let runtime = runtime(&fixture.0, config, None);
    let thread = echo_thread(&runtime).await;
    let leaf = "!tool bash {\"command\":\"printf nested > nested.txt\"}";
    let child = format!("!tool task {}", json!({"prompt":leaf,"agent":"general"}));
    let turn = send(
        &runtime,
        &thread,
        &format!("!tool task {}", json!({"prompt":child,"agent":"general"})),
    )
    .await;
    assert_success(&runtime, &thread, &turn).await;
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("nested.txt")).unwrap(),
        "nested"
    );
    let children: Vec<_> = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.parent_id.is_some())
        .collect();
    assert_eq!(children.len(), 2);
}

#[tokio::test]
async fn parity_default_depth_still_prevents_recursive_delegation() {
    let fixture = Fixture::new();
    let runtime = runtime(
        &fixture.0,
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        None,
    );
    let thread = echo_thread(&runtime).await;
    let child = format!(
        "!tool task {}",
        json!({"prompt":"!tool bash {\"command\":\"touch forbidden\"}","agent":"general"})
    );
    let turn = send(
        &runtime,
        &thread,
        &format!("!tool task {}", json!({"prompt":child,"agent":"general"})),
    )
    .await;
    wait_done(&runtime, &turn).await;
    assert!(!fixture.0.join("forbidden").exists());
    assert_eq!(runtime.list_threads(false).await.unwrap().len(), 2);
}

#[tokio::test]
async fn parity_definition_shell_asks_then_expands_stdout_and_preserves_failure() {
    let fixture = Fixture::new();
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(rule("bash:*", D::Ask));
    config.commands.push(haarness_protocol::CommandInfo {
        name: "probe".into(),
        description: "Dynamic context".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: "Context: !`printf APPROVED_CONTEXT; printf stderr-only >&2`".into(),
        plugin: None,
        execution: haarness_protocol::DefinitionExecution::default(),
    });
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(fixture.0.clone())),
        EventBus::new(2048),
        fixture.0.clone(),
        String::new(),
        config,
    );
    let thread = echo_thread(&runtime).await;
    let mut request = turn_request("");
    request.command = Some("probe".into());
    let turn = runtime.start_turn(thread.id, request).await.unwrap();
    let permission = wait_permission(&runtime, thread.id).await;
    runtime
        .reply_permission(
            &permission,
            PermissionReply {
                decision: Reply::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_success(&runtime, &thread, &turn).await;
    assert!(requests.lock().unwrap().iter().any(|request| request.messages.iter().any(|message| matches!(message,ProviderMessage::Text{role,content} if role=="user" && content.contains("Context: APPROVED_CONTEXT") && !content.contains("stderr-only")))));
}

#[tokio::test]
async fn parity_installed_helpers_are_readable_but_secrets_and_symlink_escapes_still_ask() {
    let fixture = Fixture::new();
    let ws = fixture.0.join("ws");
    let skills = fixture.0.join("skills");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("helper.py"), "helper").unwrap();
    std::fs::write(skills.join(".env"), "secret").unwrap();
    std::fs::write(fixture.0.join("outside.txt"), "outside").unwrap();
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.read_roots.push(skills.clone());
    let runtime = Arc::new(runtime(&ws, config, None));
    let thread = echo_thread(&runtime).await;
    let result = runtime
        .run_direct_tool(
            thread.id,
            "read",
            json!({"path":skills.join("helper.py")}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut checks = vec![
        ("read", json!({"path":skills.join(".env")})),
        (
            "write",
            json!({"path":skills.join("new.py"),"content":"no"}),
        ),
    ];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(fixture.0.join("outside.txt"), skills.join("linked.txt"))
            .unwrap();
        checks.push(("read", json!({"path":skills.join("linked.txt")})));
    }
    for (name, args) in checks {
        let r = runtime.clone();
        let id = thread.id;
        let call = tokio::spawn(async move {
            r.run_direct_tool(id, name, args, CancellationToken::new())
                .await
        });
        let permission = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(p) = runtime.pending_permissions().first() {
                    break p.permission_id.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        runtime
            .reply_permission(
                &permission,
                PermissionReply {
                    decision: Reply::Reject,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        assert!(call.await.unwrap().unwrap().is_error);
    }
    assert!(!skills.join("new.py").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn parity_secret_reads_follow_the_file_really_opened_and_spare_node_modules() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("node_modules/pkg")).unwrap();
    std::fs::write(fixture.0.join(".env"), "secret").unwrap();
    std::fs::write(fixture.0.join("node_modules/pkg/.env"), "fixture").unwrap();
    std::os::unix::fs::symlink(".env", fixture.0.join("notes.txt")).unwrap();
    let runtime = Arc::new(runtime(
        &fixture.0,
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        None,
    ));
    let thread = echo_thread(&runtime).await;
    // A package's .env fixture is read without asking.
    let result = runtime
        .run_direct_tool(
            thread.id,
            "read",
            json!({"path":"node_modules/pkg/.env"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.output.text.contains("fixture"));
    // A link to the project's .env, or a path that only passes through
    // node_modules, asks for the file it really opens.
    for (path, always) in [
        ("notes.txt", vec!["read:notes.txt", "read:.env"]),
        (
            "node_modules/pkg/../../.env",
            vec!["read:node_modules/pkg/../../.env", "read:.env"],
        ),
    ] {
        let r = runtime.clone();
        let id = thread.id;
        let call = tokio::spawn(async move {
            r.run_direct_tool(id, "read", json!({"path":path}), CancellationToken::new())
                .await
        });
        let approval = pending(&runtime).await;
        assert_eq!(approval.permission, "read");
        assert_eq!(approval.patterns, vec![".env"], "{path}");
        assert_eq!(approval.always_patterns, always, "{path}");
        runtime
            .reply_permission(
                &approval.permission_id,
                PermissionReply {
                    decision: Reply::Reject,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        let result = call.await.unwrap().unwrap();
        assert!(result.is_error);
        assert!(!result.output.text.contains("secret"));
    }
    // Search tools never ask; they leave the secret out and say so.
    let result = runtime
        .run_direct_tool(
            thread.id,
            "grep",
            json!({"pattern":".","path":"notes.txt"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.output.text,
        "No files found\n\n(skipped 1 file with a secret's name)"
    );
    assert!(runtime.pending_permissions().is_empty());
}

/// A hook that allows a read unless its path ends in `.env`, as Claude Code
/// hooks commonly do, logging each input it is given.
#[cfg(unix)]
fn env_guard_hooks(workspace: &Path, event: &str, log: &Path) -> Arc<dyn haarness_core::HookPort> {
    let allow = if event == "PreToolUse" {
        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#
    } else {
        r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#
    };
    let command = format!(
        r#"input=$(cat); printf '%s\n' "$input" >> '{}'; if printf '%s' "$input" | grep -q '"path":"[^"]*\.env"'; then exit 0; fi; echo '{allow}'"#,
        log.display()
    );
    Arc::new(haarness_server::ConfiguredHooks::new(
        haarness_server::ConfiguredHooks::parse_block(
            &json!({ event: [{"matcher": "Read", "hooks": [{"command": command}]}] }),
        ),
        workspace.to_owned(),
    ))
}

#[cfg(unix)]
#[tokio::test]
async fn parity_hooks_judge_the_file_a_link_really_opens() {
    for event in ["PreToolUse", "PermissionRequest"] {
        let fixture = Fixture::new();
        let log = fixture.0.join("hook.log");
        std::fs::write(fixture.0.join(".env"), "SECRET_CANARY").unwrap();
        std::os::unix::fs::symlink(".env", fixture.0.join("notes.txt")).unwrap();
        let runtime = Arc::new(
            runtime(
                &fixture.0,
                RuntimeConfig {
                    approval_mode: haarness_protocol::ApprovalMode::Review,
                    ..RuntimeConfig::default()
                },
                None,
            )
            .with_hooks(env_guard_hooks(&fixture.0, event, &log)),
        );
        let thread = echo_thread(&runtime).await;
        // The model path: the hook allows `notes.txt`, but the read opens
        // `.env`, which the hook would refuse, so the person is asked.
        let turn = send(
            &runtime,
            &thread,
            &format!("!tool read {}", json!({"path": "notes.txt"})),
        )
        .await;
        let approval = pending(&runtime).await;
        assert_eq!(approval.permission, "read", "{event}");
        assert_eq!(approval.patterns, vec![".env"], "{event}");
        runtime
            .reply_permission(
                &approval.permission_id,
                PermissionReply {
                    decision: Reply::Reject,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        wait_done(&runtime, &turn).await;
        let items = runtime.items(thread.id, None).await.unwrap();
        assert!(
            !serde_json::to_string(&items)
                .unwrap()
                .contains("SECRET_CANARY"),
            "{event}"
        );
        // A direct call (workflows, `!` shell) is asked the same way.
        if event == "PreToolUse" {
            let r = runtime.clone();
            let id = thread.id;
            let call = tokio::spawn(async move {
                r.run_direct_tool(
                    id,
                    "read",
                    json!({"path":"notes.txt"}),
                    CancellationToken::new(),
                )
                .await
            });
            let approval = pending(&runtime).await;
            assert_eq!(approval.patterns, vec![".env"]);
            runtime
                .reply_permission(
                    &approval.permission_id,
                    PermissionReply {
                        decision: Reply::Reject,
                        feedback: None,
                    },
                )
                .await
                .unwrap();
            let result = call.await.unwrap().unwrap();
            assert!(result.is_error);
            assert!(!result.output.text.contains("SECRET_CANARY"));
        } else {
            // The hook was asked about `.env` itself, by path and pattern.
            let inputs: Vec<serde_json::Value> = std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(inputs.len(), 1, "{inputs:?}");
            assert_eq!(inputs[0]["tool_input"]["path"], ".env");
            assert_eq!(inputs[0]["permission_patterns"], json!([".env"]));
        }
        assert!(runtime.pending_permissions().is_empty());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn parity_a_link_out_of_the_project_asks_although_a_hook_allowed_its_name() {
    let fixture = Fixture::new();
    let workspace = fixture.0.join("project");
    let outside = fixture.0.join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "SECRET_CANARY").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), workspace.join("notes.txt")).unwrap();
    let log = fixture.0.join("hook.log");
    let allow = r#"echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}'"#;
    let record = format!("cat >> '{}'", log.display());
    let hooks = Arc::new(haarness_server::ConfiguredHooks::new(
        haarness_server::ConfiguredHooks::parse_block(&json!({
            "PreToolUse": [{"matcher": "Read", "hooks": [{"command": allow}]}],
            "PermissionRequest": [{"hooks": [{"command": record}]}],
        })),
        workspace.clone(),
    ));
    let runtime = Arc::new(
        runtime(
            &workspace,
            RuntimeConfig {
                approval_mode: haarness_protocol::ApprovalMode::Review,
                ..RuntimeConfig::default()
            },
            None,
        )
        .with_hooks(hooks),
    );
    let thread = echo_thread(&runtime).await;
    let turn = send(
        &runtime,
        &thread,
        &format!("!tool read {}", json!({"path": "notes.txt"})),
    )
    .await;
    let approval = pending(&runtime).await;
    assert_eq!(approval.permission, "external_directory");
    assert_eq!(approval.patterns, vec![format!("{}/*", outside.display())]);
    runtime
        .reply_permission(
            &approval.permission_id,
            PermissionReply {
                decision: Reply::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    let input: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&log).unwrap().trim()).unwrap();
    assert_eq!(input["tool_name"], "external_directory");
    assert_eq!(
        input["tool_input"]["path"],
        outside.join("secret.txt").display().to_string()
    );
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        !serde_json::to_string(&items)
            .unwrap()
            .contains("SECRET_CANARY")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn parity_a_hook_that_sees_the_opened_file_can_still_answer() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join(".env"), "SECRET_CANARY").unwrap();
    std::os::unix::fs::symlink(".env", fixture.0.join("notes.txt")).unwrap();
    let allow_all = r#"echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'"#;
    let hooks = Arc::new(haarness_server::ConfiguredHooks::new(
        haarness_server::ConfiguredHooks::parse_block(
            &json!({"PermissionRequest": [{"matcher": "Read", "hooks": [{"command": allow_all}]}]}),
        ),
        fixture.0.clone(),
    ));
    let runtime = Arc::new(
        runtime(
            &fixture.0,
            RuntimeConfig {
                approval_mode: haarness_protocol::ApprovalMode::Review,
                ..RuntimeConfig::default()
            },
            None,
        )
        .with_hooks(hooks),
    );
    let thread = echo_thread(&runtime).await;
    let turn = send(
        &runtime,
        &thread,
        &format!("!tool read {}", json!({"path": "notes.txt"})),
    )
    .await;
    assert_success(&runtime, &thread, &turn).await;
    let items = runtime.items(thread.id, None).await.unwrap();
    assert!(
        serde_json::to_string(&items)
            .unwrap()
            .contains("SECRET_CANARY")
    );
}

#[tokio::test]
async fn parity_repository_grants_cross_worktrees_and_preserve_other_repositories_and_denies() {
    let fixture = Fixture::new();
    let main = fixture.0.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_fixture_command(&main, ["init", "-b", "main"]);
    git_fixture_command(
        &main,
        [
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    let linked = fixture.0.join("linked");
    git_fixture_command(
        &main,
        ["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let db = fixture.0.join("approvals.sqlite");
    let common = main.join(".git").canonicalize().unwrap();
    let store = Arc::new(haarness_store::RepositoryApprovals::open(&db, &common).unwrap());
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(rule("bash:*", D::Ask));
    let first = runtime(&main, config.clone(), None)
        .with_repository_approvals(store.clone())
        .unwrap();
    let second = runtime(&linked, config.clone(), None)
        .with_repository_approvals(Arc::new(
            haarness_store::RepositoryApprovals::open(&db, &common).unwrap(),
        ))
        .unwrap();
    let thread = echo_thread(&first).await;
    let turn = send(
        &first,
        &thread,
        "!tool bash {\"command\":\"git status --short\"}",
    )
    .await;
    let permission = wait_permission(&first, thread.id).await;
    assert_eq!(
        first.pending_permissions()[0].repository_scope.as_deref(),
        Some(common.to_str().unwrap())
    );
    first
        .reply_permission(
            &permission,
            PermissionReply {
                decision: Reply::AlwaysRepository,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_success(&first, &thread, &turn).await;
    let thread = echo_thread(&second).await;
    let turn = send(
        &second,
        &thread,
        "!tool bash {\"command\":\"git status --porcelain\"}",
    )
    .await;
    assert_success(&second, &thread, &turn).await;
    assert!(second.pending_permissions().is_empty());
    assert!(!store.load().unwrap().is_empty());
    let other =
        haarness_store::RepositoryApprovals::open(&db, &fixture.0.join("unrelated/.git")).unwrap();
    assert!(other.load().unwrap().is_empty());
    config.base_rules.push(rule("bash:*", D::Deny));
    let denied = runtime(&linked, config, None)
        .with_repository_approvals(store)
        .unwrap();
    let thread = echo_thread(&denied).await;
    let result = denied
        .run_direct_tool(
            thread.id,
            "bash",
            json!({"command":"git status"}),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn parity_invalid_repository_reply_keeps_prompt_available() {
    let fixture = Fixture::new();
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(rule("bash:*", D::Ask));
    let runtime = runtime(&fixture.0, config, None);
    let thread = echo_thread(&runtime).await;
    let turn = send(
        &runtime,
        &thread,
        "!tool bash {\"command\":\"printf retry\"}",
    )
    .await;
    let id = wait_permission(&runtime, thread.id).await;
    assert!(
        runtime
            .reply_permission(
                &id,
                PermissionReply {
                    decision: Reply::AlwaysRepository,
                    feedback: None
                }
            )
            .await
            .is_err()
    );
    assert_eq!(runtime.pending_permissions().len(), 1);
    runtime
        .reply_permission(
            &id,
            PermissionReply {
                decision: Reply::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_success(&runtime, &thread, &turn).await;
}

struct OverlayRegistry {
    native: NativeToolRegistry,
    tools: Vec<Arc<dyn Tool>>,
}
impl ToolRegistry for OverlayRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.definition().name == name)
            .cloned()
            .or_else(|| self.native.get(name))
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut all = self.native.list();
        for tool in &self.tools {
            let definition = tool.definition();
            all.retain(|other| other.name != definition.name);
            all.push(definition);
        }
        all
    }
}
struct BrowserFixture {
    forced: bool,
    executions: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl Tool for BrowserFixture {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser_files_upload".into(),
            description: "Fixture upload".into(),
            input_schema: json!({"type":"object","properties":{"paths":{"type":"array","items":{"type":"string"}}},"required":["paths"]}),
        }
    }
    fn side_effectful(&self) -> bool {
        true
    }
    async fn prepare_arguments(
        &self,
        mut args: Value,
        _thread: haarness_protocol::ThreadId,
        _cancel: CancellationToken,
    ) -> CoreResult<Value> {
        args["__browser"] = json!({"target":{"resources":["https://upload.example.test"]}});
        Ok(args)
    }
    fn permission_constraint(&self, _args: &Value, operation: &str) -> Option<D> {
        (self.forced && operation.starts_with("browser:")).then_some(D::Ask)
    }
    async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolOutput {
            text: "uploaded".into(),
            metadata: json!({}),
        })
    }
}
async fn pending(runtime: &AgentRuntime) -> haarness_protocol::PendingPermissionInfo {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(pending) = runtime.pending_permissions().first() {
                return pending.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn parity_browser_upload_combines_all_gates_and_denies_before_asking() {
    let root = Fixture::new();
    let ws = root.0.join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    let secret = root.0.join(".env");
    std::fs::write(&secret, "fixture-only").unwrap();
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.push(rule("browser:*", D::Ask));
    let tools = Arc::new(OverlayRegistry {
        native: NativeToolRegistry::new(ws.clone()),
        tools: vec![Arc::new(BrowserFixture {
            forced: false,
            executions: executions.clone(),
        })],
    });
    let runtime = Arc::new(runtime(&ws, config.clone(), Some(tools.clone())));
    let thread = echo_thread(&runtime).await;
    let r = runtime.clone();
    let path = secret.clone();
    let id = thread.id;
    let call = tokio::spawn(async move {
        r.run_direct_tool(
            id,
            "browser_files_upload",
            json!({"paths":[path]}),
            CancellationToken::new(),
        )
        .await
    });
    let approval = pending(&runtime).await;
    assert_eq!(approval.permission, "multiple");
    assert_eq!(approval.patterns.len(), 3);
    assert_eq!(approval.always_patterns.len(), 3);
    assert!(approval.patterns.iter().any(|p| p.starts_with("read:")));
    runtime
        .reply_permission(
            &approval.permission_id,
            PermissionReply {
                decision: Reply::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert!(!call.await.unwrap().unwrap().is_error);
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 1);
    config.base_rules.push(rule("read:*.env", D::Deny));
    let denied = self::runtime(&ws, config, Some(tools));
    let thread = echo_thread(&denied).await;
    assert!(
        denied
            .run_direct_tool(
                thread.id,
                "browser_files_upload",
                json!({"paths":[secret]}),
                CancellationToken::new()
            )
            .await
            .unwrap()
            .is_error
    );
    assert!(denied.pending_permissions().is_empty());
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 1);
}
#[tokio::test]
async fn parity_forced_browser_ask_is_not_skipped_by_default_allow_or_always() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("upload.txt"), "fixture").unwrap();
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools = Arc::new(OverlayRegistry {
        native: NativeToolRegistry::new(fixture.0.clone()),
        tools: vec![Arc::new(BrowserFixture {
            forced: true,
            executions: executions.clone(),
        })],
    });
    let runtime = runtime(
        &fixture.0,
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        Some(tools),
    );
    let thread = echo_thread(&runtime).await;
    for _ in 0..2 {
        let turn = send(
            &runtime,
            &thread,
            "!tool browser_files_upload {\"paths\":[\"upload.txt\"]}",
        )
        .await;
        let approval = pending(&runtime).await;
        runtime
            .reply_permission(
                &approval.permission_id,
                PermissionReply {
                    decision: Reply::Always,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        assert_success(&runtime, &thread, &turn).await;
    }
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 2);
}

struct BarrierRead {
    barrier: Arc<tokio::sync::Barrier>,
    observed: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl Tool for BarrierRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".into(),
            description: "Read at deterministic barrier".into(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    fn parallel_safe(&self, _args: &Value) -> bool {
        true
    }
    async fn execute(&self, _args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        tokio::time::timeout(Duration::from_secs(3), self.barrier.wait())
            .await
            .map_err(|_| CoreError::Tool("observations ran serially".into()))?;
        self.observed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolOutput {
            text: "observed".into(),
            metadata: json!({}),
        })
    }
}
struct OrderedWrite {
    observed: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl Tool for OrderedWrite {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: "Observe write ordering".into(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"},"expected":{"type":"integer"}},"required":["path","expected"]}),
        }
    }
    fn side_effectful(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        if self.observed.load(std::sync::atomic::Ordering::SeqCst)
            != usize::try_from(args["expected"].as_u64().unwrap()).unwrap()
        {
            return Err(CoreError::Tool(
                "write crossed an observation barrier".into(),
            ));
        }
        Ok(ToolOutput {
            text: "ordered".into(),
            metadata: json!({}),
        })
    }
}
#[tokio::test]
async fn parity_native_and_batch_reads_run_concurrently_with_write_barriers() {
    for batch in [false, true] {
        let fixture = Fixture::new();
        let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tools = Arc::new(OverlayRegistry {
            native: NativeToolRegistry::new(fixture.0.clone()),
            tools: vec![
                Arc::new(BarrierRead {
                    barrier: Arc::new(tokio::sync::Barrier::new(2)),
                    observed: observed.clone(),
                }),
                Arc::new(OrderedWrite {
                    observed: observed.clone(),
                }),
            ],
        });
        let mut config = RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        };
        config.execution.read_parallelism = 2;
        let runtime = runtime(&fixture.0, config, Some(tools));
        let thread = echo_thread(&runtime).await;
        let calls = [
            ("read", json!({"path":"one"})),
            ("read", json!({"path":"two"})),
            ("write", json!({"path":"out","expected":2})),
            ("read", json!({"path":"three"})),
            ("read", json!({"path":"four"})),
            ("write", json!({"path":"out","expected":4})),
        ];
        let script = if batch {
            format!(
                "!tool batch {}",
                json!({"tool_calls":calls.iter().map(|(name,args)|json!({"tool":name,"parameters":args})).collect::<Vec<_>>()})
            )
        } else {
            format!(
                "!multi {}",
                json!(
                    calls
                        .iter()
                        .map(|(name, args)| json!({"name":name,"arguments":args}))
                        .collect::<Vec<_>>()
                )
            )
        };
        let turn = send(&runtime, &thread, &script).await;
        assert_success(&runtime, &thread, &turn).await;
        assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), 4);
        let items = runtime.items(thread.id, None).await.unwrap();
        if batch {
            assert!(items.iter().any(|item|matches!(&item.data,ItemData::ToolResult{metadata,..}if metadata["failures"]==0)));
        } else {
            let names = items
                .iter()
                .filter_map(|item| {
                    if let ItemData::ToolResult { name, .. } = &item.data {
                        Some(name.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(names, ["read", "read", "write", "read", "read", "write"]);
        }
    }
}

/// A read that holds `slow` until released, so a sibling of its parallel
/// group asks while it still runs.
struct HeldRead {
    release: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl Tool for HeldRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".into(),
            description: "Read that holds until released".into(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    fn parallel_safe(&self, _args: &Value) -> bool {
        true
    }
    async fn execute(&self, args: Value, _context: ToolContext) -> CoreResult<ToolOutput> {
        if args["path"] == "slow" {
            self.release.notified().await;
        }
        Ok(ToolOutput {
            text: "read".into(),
            metadata: json!({}),
        })
    }
}

/// In a parallel group the request, its listing and the waiting status all
/// name the call that asks, not the earlier read still running beside it;
/// a batch's step by `<batch call id>:N`.
#[tokio::test]
async fn parity_a_request_names_the_call_that_asks() {
    for batch in [false, true] {
        let fixture = Fixture::new();
        let release = Arc::new(tokio::sync::Notify::new());
        let tools = Arc::new(OverlayRegistry {
            native: NativeToolRegistry::new(fixture.0.clone()),
            tools: vec![Arc::new(HeldRead {
                release: release.clone(),
            })],
        });
        let mut config = RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        };
        config.base_rules.push(rule("read:*asks", D::Ask));
        let runtime = runtime(&fixture.0, config, Some(tools));
        let thread = echo_thread(&runtime).await;
        let mut events = runtime.events().subscribe(None).receiver;
        let calls = [json!({"path":"slow"}), json!({"path":"asks"})];
        let script = if batch {
            format!(
                "!tool batch {}",
                json!({"tool_calls":calls.iter().map(|args|json!({"tool":"read","parameters":args})).collect::<Vec<_>>()})
            )
        } else {
            format!(
                "!multi {}",
                json!(
                    calls
                        .iter()
                        .map(|args| json!({"name":"read","arguments":args}))
                        .collect::<Vec<_>>()
                )
            )
        };
        let turn = send(&runtime, &thread, &script).await;
        let asked = pending(&runtime).await;
        let items = runtime.items(thread.id, None).await.unwrap();
        let call_ids: Vec<&str> = items
            .iter()
            .filter_map(|item| match &item.data {
                ItemData::ToolCall { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        let expected = if batch {
            format!("{}:2", call_ids[0])
        } else {
            call_ids[1].to_owned()
        };
        assert_eq!(asked.call_id.as_deref(), Some(expected.as_str()), "{batch}");
        // The slow read still runs: no call has its result yet.
        assert!(
            !items
                .iter()
                .any(|item| matches!(item.data, ItemData::ToolResult { .. })),
            "{batch}"
        );
        let (mut requested, mut waiting) = (None, None);
        while requested.is_none() || waiting.is_none() {
            let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap()
                .unwrap();
            match event.event_type.as_str() {
                "permission.requested" => requested = Some(event.payload["callId"].clone()),
                "turn.status" if event.payload["state"] == "waiting_permission" => {
                    assert_eq!(event.payload["tool"], "read", "{batch}");
                    waiting = Some(event.payload["callId"].clone());
                }
                _ => {}
            }
        }
        assert_eq!(requested, Some(json!(expected)), "{batch}");
        assert_eq!(waiting, Some(json!(expected)), "{batch}");
        runtime
            .reply_permission(
                &asked.permission_id,
                PermissionReply {
                    decision: Reply::Once,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        release.notify_one();
        assert_success(&runtime, &thread, &turn).await;
    }
}

/// A runtime whose one command (`probe`) runs `template` with `execution`.
#[cfg(unix)]
fn snippet_runtime(
    workspace: &Path,
    rules: Vec<PermissionRule>,
    template: &str,
    execution: haarness_protocol::DefinitionExecution,
) -> (AgentRuntime, Arc<std::sync::Mutex<Vec<ProviderRequest>>>) {
    let mut config = RuntimeConfig {
        approval_mode: haarness_protocol::ApprovalMode::Review,
        ..RuntimeConfig::default()
    };
    config.base_rules.extend(rules);
    config.commands.push(haarness_protocol::CommandInfo {
        name: "probe".into(),
        description: "Snippet permissions".into(),
        source: None,
        agent: None,
        model: None,
        argument_hint: None,
        template: template.into(),
        plugin: None,
        execution,
    });
    let provider = Capturing::default();
    let requests = provider.requests.clone();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider) as Arc<dyn ModelProvider>
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_owned())),
        EventBus::new(2048),
        workspace.to_owned(),
        String::new(),
        config,
    );
    (runtime, requests)
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(unix)]
async fn probe(runtime: &AgentRuntime, thread: &Thread, arguments: &str) -> Turn {
    let mut request = turn_request(arguments);
    request.command = Some("probe".into());
    runtime.start_turn(thread.id, request).await.unwrap()
}

#[cfg(unix)]
fn permission_request(
    items: &[haarness_protocol::Item],
) -> Option<(String, Vec<String>, Vec<String>)> {
    items.iter().find_map(|item| match &item.data {
        ItemData::PermissionRequest {
            permission,
            patterns,
            always_patterns,
            ..
        } => Some((
            permission.clone(),
            patterns.clone(),
            always_patterns.clone(),
        )),
        _ => None,
    })
}

/// A snippet is judged on its text with the folders filled in, as a person
/// reads it: Always saves the real path, a rule naming the real path still
/// allows it, and a path that leaves the project through a placeholder asks.
#[cfg(unix)]
#[tokio::test]
async fn parity_a_snippet_is_judged_with_its_folders_filled_in() {
    let fixture = Fixture::new();
    let workspace = fixture.0.join("ws");
    let skill = workspace.join("skills/tool");
    script(&skill.join("run.sh"), "printf SKILL_RAN");
    std::fs::write(fixture.0.join("outside.txt"), "OUTSIDE_SECRET").unwrap();
    let execution = haarness_protocol::DefinitionExecution {
        directory: Some(skill.display().to_string()),
        ..Default::default()
    };
    let run = skill.join("run.sh").display().to_string();

    // The card and an Always answer name the real path.
    let (runtime, _) = snippet_runtime(
        &workspace,
        vec![rule("bash:*", D::Ask)],
        "Ctx: !`${CLAUDE_SKILL_DIR}/run.sh go`",
        execution.clone(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = probe(&runtime, &thread, "").await;
    let permission = wait_permission(&runtime, thread.id).await;
    let (_, patterns, always) =
        permission_request(&runtime.items(thread.id, None).await.unwrap()).unwrap();
    assert!(
        patterns.iter().any(|pattern| pattern.contains(&run)),
        "{patterns:?}"
    );
    assert!(
        !always.is_empty() && always.iter().all(|pattern| !pattern.contains("${")),
        "{always:?}"
    );
    assert!(
        always.iter().any(|pattern| pattern.contains(&run)),
        "{always:?}"
    );
    runtime
        .reply_permission(
            &permission,
            PermissionReply {
                decision: Reply::Once,
                feedback: None,
            },
        )
        .await
        .unwrap();
    assert_success(&runtime, &thread, &turn).await;

    // A rule a person saved earlier for the real path still allows it.
    let (runtime, requests) = snippet_runtime(
        &workspace,
        vec![
            rule("bash:*", D::Ask),
            rule(&format!("bash:{run} *"), D::Allow),
        ],
        "Ctx: !`${CLAUDE_SKILL_DIR}/run.sh go`",
        execution.clone(),
    );
    let thread = echo_thread(&runtime).await;
    let turn = probe(&runtime, &thread, "").await;
    assert_success(&runtime, &thread, &turn).await;
    assert!(permission_request(&runtime.items(thread.id, None).await.unwrap()).is_none());
    assert!(requests.lock().unwrap().iter().any(|request| request.messages.iter().any(
        |message| matches!(message, ProviderMessage::Text { role, content } if role == "user" && content.contains("Ctx: SKILL_RAN"))
    )));

    // A path that leaves the project through the skill's folder asks.
    let (runtime, requests) = snippet_runtime(
        &workspace,
        Vec::new(),
        "Ctx: !`cat \"${CLAUDE_SKILL_DIR}/../../../outside.txt\"`",
        execution,
    );
    let thread = echo_thread(&runtime).await;
    let turn = probe(&runtime, &thread, "").await;
    let permission = wait_permission(&runtime, thread.id).await;
    let (kind, _, _) = permission_request(&runtime.items(thread.id, None).await.unwrap()).unwrap();
    assert_eq!(kind, "external_directory");
    runtime
        .reply_permission(
            &permission,
            PermissionReply {
                decision: Reply::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(&runtime, &turn).await;
    assert!(!requests.lock().unwrap().iter().any(|request| request.messages.iter().any(
        |message| matches!(message, ProviderMessage::Text { content, .. } if content.contains("OUTSIDE_SECRET"))
    )));
}

/// What the turn's tool calls returned, in order.
async fn tool_outputs(runtime: &AgentRuntime, thread: &Thread) -> Vec<(String, bool)> {
    runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::ToolResult {
                output, is_error, ..
            } => Some((output, is_error)),
            _ => None,
        })
        .collect()
}

async fn answer(runtime: &AgentRuntime, id: &str, decision: Reply) {
    runtime
        .reply_permission(
            id,
            PermissionReply {
                decision,
                feedback: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn parity_always_on_a_folder_that_holds_the_project_remembers_only_the_files_asked_about() {
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    let project = home.join("work/app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(home.join(".gitconfig"), "[user]\n\tname = Fixture\n").unwrap();
    std::fs::write(home.join(".netrc"), "machine example.test\n").unwrap();
    let runtime = runtime(
        &project,
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Review,
            ..RuntimeConfig::default()
        },
        None,
    );
    let thread = echo_thread(&runtime).await;
    let gitconfig = home.join(".gitconfig").display().to_string();
    // The file is asked about by itself, and Always offers to remember it
    // alone.
    let turn = send(
        &runtime,
        &thread,
        "!tool bash {\"command\":\"cat ../../.gitconfig\"}",
    )
    .await;
    let asked = pending(&runtime).await;
    assert_eq!(asked.permission, "external_directory");
    assert_eq!(asked.patterns, vec![gitconfig.clone()]);
    assert_eq!(
        asked.always_patterns,
        vec![format!("external_directory:{gitconfig}")]
    );
    assert_eq!(asked.allow_always, None);
    answer(&runtime, &asked.permission_id, Reply::AlwaysWorkspace).await;
    assert_success(&runtime, &thread, &turn).await;
    // The same file again runs without a question; another file beside the
    // project still asks.
    let turn = send(
        &runtime,
        &thread,
        "!tool bash {\"command\":\"cat ../../.gitconfig\"}",
    )
    .await;
    assert_success(&runtime, &thread, &turn).await;
    let turn = send(
        &runtime,
        &thread,
        "!tool bash {\"command\":\"cat ../../.netrc\"}",
    )
    .await;
    let asked = pending(&runtime).await;
    assert_eq!(
        asked.patterns,
        vec![home.join(".netrc").display().to_string()]
    );
    answer(&runtime, &asked.permission_id, Reply::Once).await;
    assert_success(&runtime, &thread, &turn).await;
    // The folder named as a folder is asked about, and remembered, alone:
    // that runs again without a question, and opens no file in it.
    let itself = format!("{}/", home.display());
    let list = "!tool bash {\"command\":\"cd ../.. && ls -a\"}";
    let turn = send(&runtime, &thread, list).await;
    let asked = pending(&runtime).await;
    assert_eq!(asked.patterns, vec![itself.clone()]);
    assert_eq!(
        asked.always_patterns,
        vec![format!("external_directory:{itself}")]
    );
    assert_eq!(asked.allow_always, None);
    answer(&runtime, &asked.permission_id, Reply::AlwaysWorkspace).await;
    assert_success(&runtime, &thread, &turn).await;
    let turn = send(&runtime, &thread, list).await;
    assert_success(&runtime, &thread, &turn).await;
    let turn = send(
        &runtime,
        &thread,
        "!tool bash {\"command\":\"cat ../../.netrc\"}",
    )
    .await;
    let asked = pending(&runtime).await;
    assert_eq!(
        asked.patterns,
        vec![home.join(".netrc").display().to_string()]
    );
    answer(&runtime, &asked.permission_id, Reply::Once).await;
    assert_success(&runtime, &thread, &turn).await;
    let outputs = tool_outputs(&runtime, &thread).await;
    assert!(outputs[0].0.contains("name = Fixture"), "{outputs:?}");
    assert!(outputs[3].0.contains(".netrc"), "{outputs:?}");
    assert!(outputs[5].0.contains("example.test"), "{outputs:?}");
}

#[tokio::test]
async fn parity_a_read_of_where_sign_ins_are_kept_asks_a_person_even_in_yolo() {
    let fixture = Fixture::new();
    let data = fixture.0.join("data");
    let project = fixture.0.join("project");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let store = data.join("auth.json");
    std::fs::write(&store, "{\"fixture\":{\"key\":\"sk-fixture-secret\"}}\n").unwrap();
    std::fs::write(data.join("notes.md"), "sk-fixture-secret is not here\n").unwrap();
    haarness_core::record_credential_store(store.clone(), false);
    let runtime = runtime(
        &project,
        RuntimeConfig {
            approval_mode: haarness_protocol::ApprovalMode::Yolo,
            ..RuntimeConfig::default()
        },
        None,
    );
    let thread = echo_thread(&runtime).await;
    let read = format!(
        "!tool read {}",
        json!({ "path": store.display().to_string() })
    );
    let cat = format!(
        "!tool bash {}",
        json!({ "command": format!("cat '{}'", store.display()) })
    );
    // A change of it through the file tools, too.
    let write = format!(
        "!tool write {}",
        json!({ "path": store.display().to_string(), "content": "{}" })
    );
    for script in [&read, &cat, &write] {
        let turn = send(&runtime, &thread, script).await;
        let asked = pending(&runtime).await;
        assert_eq!(asked.allow_always, Some(false), "{script}");
        answer(&runtime, &asked.permission_id, Reply::Reject).await;
        assert_eq!(
            wait_done(&runtime, &turn).await.status,
            TurnStatus::Completed
        );
    }
    // A search of its folder runs, and leaves it out.
    let turn = send(
        &runtime,
        &thread,
        &format!(
            "!tool grep {}",
            json!({ "pattern": "sk-fixture-secret", "path": data.display().to_string() })
        ),
    )
    .await;
    assert_eq!(
        wait_done(&runtime, &turn).await.status,
        TurnStatus::Completed
    );
    let outputs = tool_outputs(&runtime, &thread).await;
    let (searched, failed) = outputs.last().unwrap();
    assert!(!failed, "{searched}");
    assert!(searched.contains("notes.md"), "{searched}");
    assert!(
        searched.contains("where Haarness keeps sign-ins"),
        "{searched}"
    );
    assert!(
        !outputs
            .iter()
            .any(|(output, _)| output.contains("\"key\":\"sk-fixture-secret\"")),
        "{outputs:?}"
    );
    assert!(
        std::fs::read_to_string(&store)
            .unwrap()
            .contains("sk-fixture-secret")
    );
}

/// A site whose `/login` sends a visitor on to the page its `next` names,
/// recording each path asked for.
async fn redirecting_site() -> (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = paths.clone();
    let site = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => head.extend_from_slice(&buffer[..count]),
                }
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
            seen.lock().unwrap().push(path.clone());
            let reply = match path.split_once("?next=") {
                Some(("/login", next)) => format!(
                    "HTTP/1.1 302 Found\r\nLocation: {next}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                _ => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\npage {path}",
                    path.len() + 5
                ),
            };
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (address, paths, site)
}

#[tokio::test]
async fn parity_webfetch_redirects_meet_the_rules_in_turns_and_direct_calls() {
    let fixture = Fixture::new();
    let (address, paths, site) = redirecting_site().await;
    let mut config = RuntimeConfig::default();
    config
        .base_rules
        .push(rule(&format!("webfetch:http://{address}/admin/*"), D::Deny));
    let runtime = runtime(&fixture.0, config, None);
    let thread = echo_thread(&runtime).await;
    let stopped = format!(
        "redirects (HTTP 302) to http://{address}/admin/x, which a permission rule denies, so the \
         redirect was not followed."
    );
    for next in ["/admin/x", "/home"] {
        let url = format!("http://{address}/login?next={next}");
        let turn = send(
            &runtime,
            &thread,
            &format!("!tool webfetch {}", json!({ "url": url })),
        )
        .await;
        assert_success(&runtime, &thread, &turn).await;
        let (turned, _) = tool_outputs(&runtime, &thread).await.pop().unwrap();
        // A workflow's own fetch, outside a turn, meets the same rules.
        let direct = runtime
            .run_direct_tool(
                thread.id,
                "webfetch",
                json!({ "url": url }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!direct.is_error, "{}", direct.output.text);
        for output in [turned, direct.output.text] {
            if next == "/admin/x" {
                assert!(output.contains(&stopped), "{output}");
            } else {
                assert_eq!(output, "page /home");
            }
        }
    }
    assert!(
        !paths.lock().unwrap().iter().any(|path| path == "/admin/x"),
        "{:?}",
        paths.lock().unwrap()
    );
    assert_eq!(
        paths
            .lock()
            .unwrap()
            .iter()
            .filter(|path| *path == "/home")
            .count(),
        2
    );
    site.abort();
}

#[tokio::test]
async fn parity_searches_leave_out_files_a_read_rule_denies_in_turns_and_direct_calls() {
    let fixture = Fixture::new();
    for (file, text) in [
        ("private/key.txt", "needle private"),
        ("public/readme.md", "needle public"),
    ] {
        let path = fixture.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut config = RuntimeConfig::default();
    config.base_rules.push(rule("read:private/*", D::Deny));
    let runtime = runtime(&fixture.0, config, None);
    let thread = echo_thread(&runtime).await;
    for (name, arguments, found) in [
        (
            "grep",
            json!({"pattern": "needle", "path": "."}),
            "public/readme.md:1:needle public",
        ),
        ("glob", json!({"pattern": "**/*.*"}), "public/readme.md"),
    ] {
        let turn = send(&runtime, &thread, &format!("!tool {name} {arguments}")).await;
        assert_success(&runtime, &thread, &turn).await;
        let (turned, _) = tool_outputs(&runtime, &thread).await.pop().unwrap();
        // A workflow's own search, outside a turn, meets the same rules.
        let direct = runtime
            .run_direct_tool(thread.id, name, arguments, CancellationToken::new())
            .await
            .unwrap();
        assert!(!direct.is_error, "{}", direct.output.text);
        for output in [turned, direct.output.text] {
            assert!(output.contains(found), "{name}: {output}");
            assert!(!output.contains("key.txt"), "{name}: {output}");
            assert!(
                output.ends_with("(skipped 1 file a read rule denies)"),
                "{name}: {output}"
            );
        }
    }
    // A direct read of the file itself is refused before it runs.
    let read = runtime
        .run_direct_tool(
            thread.id,
            "read",
            json!({"path": "private/key.txt"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(read.is_error, "{}", read.output.text);
    assert!(!read.output.text.contains("needle"), "{}", read.output.text);
}

#[tokio::test]
async fn parity_searches_leave_out_files_an_active_skill_denies_reading() {
    let fixture = Fixture::new();
    for (file, text) in [
        ("private/key.txt", "needle private"),
        ("public/readme.md", "needle public"),
    ] {
        let path = fixture.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut config = RuntimeConfig::default();
    for (name, call) in [
        ("search-grep", json!({"pattern": "needle", "path": "."})),
        ("search-glob", json!({"pattern": "**/*.*"})),
    ] {
        let tool = name.trim_start_matches("search-");
        config.commands.push(haarness_protocol::CommandInfo {
            execution: haarness_protocol::DefinitionExecution {
                disallowed_tools: vec!["Read(private/*)".into()],
                ..haarness_protocol::DefinitionExecution::default()
            },
            name: name.into(),
            description: "search without the private folder".into(),
            source: None,
            agent: None,
            model: None,
            argument_hint: None,
            template: format!("!tool {tool} {call}"),
            plugin: None,
        });
    }
    let runtime = runtime(&fixture.0, config, None);
    let thread = echo_thread(&runtime).await;
    for name in ["search-grep", "search-glob"] {
        let turn = runtime
            .start_turn(
                thread.id,
                StartTurnRequest {
                    command: Some(name.into()),
                    ..turn_request("")
                },
            )
            .await
            .unwrap();
        assert_success(&runtime, &thread, &turn).await;
        let (output, _) = tool_outputs(&runtime, &thread).await.pop().unwrap();
        assert!(output.contains("public/readme.md"), "{name}: {output}");
        assert!(!output.contains("key.txt"), "{name}: {output}");
        assert!(
            output.ends_with("(skipped 1 file a read rule denies)"),
            "{name}: {output}"
        );
    }
}

#[tokio::test]
async fn parity_a_command_that_denies_reading_still_brings_its_own_files() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("src")).unwrap();
    std::fs::write(fixture.0.join("src/lib.txt"), "MARK_LIB_CONTENT needle").unwrap();
    let mut config = RuntimeConfig::default();
    for (name, template) in [
        ("given", "Review only this file: @src/lib.txt".to_owned()),
        (
            "search-none",
            format!("!tool grep {}", json!({"pattern": "needle", "path": "."})),
        ),
    ] {
        config.commands.push(haarness_protocol::CommandInfo {
            execution: haarness_protocol::DefinitionExecution {
                disallowed_tools: vec!["Read".into()],
                ..haarness_protocol::DefinitionExecution::default()
            },
            name: name.into(),
            description: "review only the files given".into(),
            source: None,
            agent: None,
            model: None,
            argument_hint: None,
            template,
            plugin: None,
        });
    }
    let runtime = runtime(&fixture.0, config, None);
    let thread = echo_thread(&runtime).await;
    let run = |name: &'static str| {
        let runtime = &runtime;
        let thread = &thread;
        async move {
            let turn = runtime
                .start_turn(
                    thread.id,
                    StartTurnRequest {
                        command: Some(name.into()),
                        ..turn_request("")
                    },
                )
                .await
                .unwrap();
            assert_success(runtime, thread, &turn).await;
        }
    };
    // The command's own `@` file comes with it, though it takes reading
    // away from the turn.
    run("given").await;
    let items = serde_json::to_string(&runtime.items(thread.id, None).await.unwrap()).unwrap();
    assert!(items.contains("MARK_LIB_CONTENT"), "{items}");
    assert!(!items.contains("File context omitted"), "{items}");
    // A search in such a turn finds nothing to read.
    run("search-none").await;
    let (output, _) = tool_outputs(&runtime, &thread).await.pop().unwrap();
    assert!(!output.contains("lib.txt"), "{output}");
    assert!(
        output.ends_with("(skipped 1 file a read rule denies)"),
        "{output}"
    );
}
