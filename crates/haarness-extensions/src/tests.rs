use super::*;
use haarness_core::EmptyToolRegistry;
use haarness_protocol::TurnId;

fn fixture(body: &str) -> (tempfile::TempDir, Config) {
    bun().expect("extension tests require Bun; install Bun or set HAARNESS_BUN");
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("index.ts");
    std::fs::write(&source, body).unwrap();
    let config = Config {
        enabled: true,
        log_stderr: false,
        timeout_ms: 2_000,
        entries: vec![Entry {
            name: None,
            source,
            options: json!({"greeting":"hello"}),
            enabled: true,
        }],
        ..Config::default()
    };
    (root, config)
}
fn context(workspace: &Path) -> ToolContext {
    ToolContext {
        execution: haarness_core::ExecutionSettings::default(),
        thread_id: ThreadId::new(),
        turn_id: TurnId::new(),
        workspace: workspace.to_path_buf(),
        authorized_external_paths: Vec::new(),
        cancel: CancellationToken::new(),
        invoker: None,
    }
}
const GOOD: &str = r"
import type { Extension } from './api';
let calls: number = 0;
export default (ctx) => ({
  apiVersion: 1, name: 'fixture',
  tools: { greet: {
    description: 'Greeting', parameters: {type:'object',properties:{name:{type:'string'}},required:['name'],additionalProperties:false},
    execute(args, context) { console.log('console cannot corrupt RPC'); return {text:`${ctx.options.greeting} ${args.name} ${++calls}`, metadata:{workspace:context.workspace}}; }
  }},
  hooks: {
    prompt: () => ({context:'PROMPT_CONTEXT'}),
    chat: () => ({context:'SYSTEM_CONTEXT', reasoningEffort:'high'}),
    'tool.before': (input) => input.arguments.name === 'blocked' ? {block:'DENIED_BY_EXTENSION'} : {arguments:{...input.arguments,name:'rewritten'}},
    'tool.after': () => ({context:'RESULT_CONTEXT'})
  }
});
";

#[tokio::test]
async fn extensions_can_expose_more_than_sixty_four_tools() {
    let (root, config) = fixture(
        r"
export default () => ({apiVersion:1, name:'large', tools:Object.fromEntries(
  Array.from({length:80}, (_,i) => ['tool_'+i, {
    description:'A catalog tool', parameters:{type:'object'}, execute:()=>({text:'ok'})
  }])
)});
",
    );
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    assert_eq!(tools.list().len(), 80);
    assert!(tools.get("ext__large__tool_79").is_some());
}

#[tokio::test]
async fn typed_tools_hooks_state_and_schema_use_real_bun() {
    let (root, config) = fixture(GOOD);
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    let tool = tools.get("ext__fixture__greet").unwrap();
    assert!(tool.side_effectful());
    assert!(
        tool.execute(json!({"name":123}), context(root.path()))
            .await
            .is_err()
    );
    let hooks = extensions.wrap_hooks(None);
    let thread = ThreadId::new();
    let decision = hooks
        .pre_tool_use(thread, "ext__fixture__greet", &json!({"name":"original"}))
        .await;
    let output = tool
        .execute(decision.updated_input.unwrap(), context(root.path()))
        .await
        .unwrap();
    assert_eq!(output.text, "hello rewritten 1");
    assert_eq!(
        tool.execute(json!({"name":"again"}), context(root.path()))
            .await
            .unwrap()
            .text,
        "hello again 2"
    );
    assert_eq!(
        hooks
            .user_prompt_submit(thread, "hello")
            .await
            .context
            .as_deref(),
        Some("PROMPT_CONTEXT")
    );
    assert_eq!(
        hooks
            .pre_tool_use(thread, "ext__fixture__greet", &json!({"name":"blocked"}))
            .await
            .block
            .as_deref(),
        Some("DENIED_BY_EXTENSION")
    );
    let mut request = ProviderRequest {
        model: "fixture-model".into(),
        system: "BASE".into(),
        messages: vec![],
        tools: vec![],
        reasoning_effort: None,
        parameters: haarness_core::RequestParameters::default(),
        output_schema: None,
    };
    hooks.prepare_request(thread, &mut request).await.unwrap();
    assert_eq!(request.system, "BASE\nSYSTEM_CONTEXT");
    assert_eq!(request.reasoning_effort.as_deref(), Some("high"));
}

/// Command hooks that object once a tool ran.
struct Objects;
#[async_trait]
impl HookPort for Objects {
    async fn pre_tool_use(&self, _: ThreadId, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: ThreadId,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision {
            block: Some("FORMAT_FEEDBACK".into()),
            context: Some("COMMAND_CONTEXT".into()),
            ..HookDecision::default()
        }
    }
    async fn user_prompt_submit(&self, _: ThreadId, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: ThreadId,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn stop(&self, _: ThreadId) {}
}

#[tokio::test]
async fn command_hook_feedback_after_a_tool_passes_extensions_and_never_stops_them() {
    let (root, config) = fixture(GOOD);
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let hooks = extensions.wrap_hooks(Some(Arc::new(Objects)));
    let decision = hooks
        .post_tool_use(ThreadId::new(), "read", &json!({"path":"a"}), "text", false)
        .await;
    assert_eq!(decision.block.as_deref(), Some("FORMAT_FEEDBACK"));
    assert_eq!(
        decision.context.as_deref(),
        Some("COMMAND_CONTEXT\nRESULT_CONTEXT")
    );
}

#[tokio::test]
async fn a_throwing_event_observer_never_skips_or_fails_the_real_hook() {
    let (root, config) = fixture(
        r"
export default () => ({apiVersion:1, name:'observer', hooks: {
  event: () => { throw new Error('observer broke'); },
  prompt: () => ({context:'STILL_RUNS'}),
  chat: () => ({context:'CHAT_STILL_RUNS'}),
  'tool.before': () => ({})
}});
",
    );
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let hooks = extensions.wrap_hooks(None);
    let thread = ThreadId::new();
    assert_eq!(
        hooks
            .user_prompt_submit(thread, "hello")
            .await
            .context
            .as_deref(),
        Some("STILL_RUNS")
    );
    assert!(
        hooks
            .pre_tool_use(thread, "read", &json!({"path":"a"}))
            .await
            .block
            .is_none()
    );
    let mut request = ProviderRequest {
        model: "fixture-model".into(),
        system: "BASE".into(),
        messages: vec![],
        tools: vec![],
        reasoning_effort: None,
        parameters: haarness_core::RequestParameters::default(),
        output_schema: None,
    };
    hooks.prepare_request(thread, &mut request).await.unwrap();
    assert_eq!(request.system, "BASE\nCHAT_STILL_RUNS");
}

#[tokio::test]
async fn disabled_extensions_never_execute_even_invalid_sources() {
    let config = Config {
        enabled: false,
        entries: vec![Entry {
            name: None,
            source: PathBuf::from("/missing/extension.ts"),
            options: Value::Null,
            enabled: true,
        }],
        ..Config::default()
    };
    assert!(
        Extensions::load(&config, Path::new("/missing"))
            .await
            .unwrap()
            .names()
            .is_empty()
    );
}

#[tokio::test]
async fn duplicate_names_and_invalid_schemas_are_rejected() {
    let (root, mut config) = fixture(GOOD);
    config.entries.push(config.entries[0].clone());
    assert!(
        Extensions::load(&config, root.path())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate")
    );
    config.entries.pop();
    std::fs::write(&config.entries[0].source, "export default {apiVersion:1,name:'bad',tools:{oops:{description:'broken',parameters:{type:'nonsense'},execute(){return ''}}}}").unwrap();
    assert!(Extensions::load(&config, root.path()).await.is_err());
}

#[tokio::test]
async fn tool_exceptions_do_not_poison_the_extension() {
    let (root, config) = fixture(
        "let count=0; export default {apiVersion:1,name:'fixture',tools:{call:{description:'call',parameters:{type:'object'},execute(){if(++count===1)throw new Error('expected failure');return 'RECOVERED'}}}}",
    );
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    let tool = tools.get("ext__fixture__call").unwrap();
    assert!(
        tool.execute(json!({}), context(root.path()))
            .await
            .unwrap_err()
            .to_string()
            .contains("expected failure")
    );
    assert_eq!(
        tool.execute(json!({}), context(root.path()))
            .await
            .unwrap()
            .text,
        "RECOVERED"
    );
}

#[tokio::test]
async fn timeout_kills_host_without_replaying_side_effects() {
    let (root, mut config) = fixture(
        "import {writeFileSync} from 'node:fs'; export default {apiVersion:1,name:'fixture',tools:{call:{description:'call',parameters:{type:'object'},execute(_,ctx){writeFileSync(ctx.workspace+'/started','once');while(true){}}}}}",
    );
    // This limit covers cold Bun startup as well as execution. Keep enough
    // headroom for a loaded build host while still bounding the infinite loop.
    config.timeout_ms = 2_000;
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    let tool = tools.get("ext__fixture__call").unwrap();
    assert!(
        tool.execute(json!({}), context(root.path()))
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("started")).unwrap(),
        "once"
    );
    assert!(
        tool.execute(json!({}), context(root.path()))
            .await
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
}

#[tokio::test]
async fn cancellation_and_crashes_are_contained() {
    for body in ["await new Promise(()=>{})", "process.exit(7)"] {
        let (root, config) = fixture(&format!(
            "export default {{apiVersion:1,name:'fixture',tools:{{call:{{description:'call',parameters:{{type:'object'}},async execute(){{{body};return ''}}}}}}}}"
        ));
        let extensions = Extensions::load(&config, root.path()).await.unwrap();
        let tools = extensions
            .wrap_tools(Arc::new(EmptyToolRegistry))
            .await
            .unwrap();
        let tool = tools.get("ext__fixture__call").unwrap();
        let context = context(root.path());
        let cancel = context.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        assert!(tool.execute(json!({}), context).await.is_err());
    }
}

#[tokio::test]
async fn workspace_bunfig_cannot_preload_code() {
    let (root, config) = fixture(GOOD);
    std::fs::write(
        root.path().join("bunfig.toml"),
        "preload = [\"./preload.ts\"]",
    )
    .unwrap();
    std::fs::write(
        root.path().join("preload.ts"),
        "import {writeFileSync} from 'node:fs';writeFileSync('UNEXPECTED_PRELOAD','bad');",
    )
    .unwrap();
    Extensions::load(&config, root.path()).await.unwrap();
    assert!(!root.path().join("UNEXPECTED_PRELOAD").exists());
}

#[tokio::test]
async fn oversized_tool_output_is_bounded() {
    let (root, config) = fixture(
        "export default {apiVersion:1,name:'fixture',tools:{call:{description:'call',parameters:{type:'object'},execute(){return 'x'.repeat(3*1024*1024)}}}}",
    );
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    assert!(
        tools
            .get("ext__fixture__call")
            .unwrap()
            .execute(json!({}), context(root.path()))
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_tool_terminates_its_spawned_process_group() {
    let (root, config) = fixture(
        "import {writeFileSync} from 'node:fs'; export default {apiVersion:1,name:'fixture',tools:{call:{description:'call',parameters:{type:'object'},async execute(_,ctx){const child=Bun.spawn(['sh','-c','exec sleep 60']);writeFileSync(ctx.workspace+'/child.pid',String(child.pid));await new Promise(()=>{});return ''}}}}",
    );
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    let tool = tools.get("ext__fixture__call").unwrap();
    let context = context(root.path());
    let cancel = context.cancel.clone();
    let call = tokio::spawn(async move { tool.execute(json!({}), context).await });
    let path = root.path().join("child.pid");
    for _ in 0..100 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid: i32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
    cancel.cancel();
    assert!(matches!(call.await.unwrap(), Err(CoreError::Cancelled)));
    for _ in 0..100 {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("extension child remained alive after cancellation");
}

#[tokio::test]
async fn separate_deadlines_and_larger_payloads_are_configurable() {
    let (root, mut config) = fixture(
        "export default async () => {await Bun.sleep(50);return {apiVersion:1,name:'fixture',tools:{call:{description:'call',parameters:{type:'object'},async execute(){await Bun.sleep(50);return 'x'.repeat(3*1024*1024)}}}}}",
    );
    config.timeout_ms = 1;
    config.init_timeout_ms = Some(2000);
    config.tool_timeout_ms = Some(0);
    config.hook_timeout_ms = Some(100);
    config.payload_max_bytes = 4 * 1024 * 1024;
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    assert_eq!(
        tools
            .get("ext__fixture__call")
            .unwrap()
            .execute(json!({}), context(root.path()))
            .await
            .unwrap()
            .text
            .len(),
        3 * 1024 * 1024
    );
    config.init_timeout_ms = Some(1);
    assert!(
        Extensions::load(&config, root.path())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("initialization timed out")
    );
}

const LIFECYCLE: &str = r"
import {appendFileSync, writeFileSync} from 'node:fs';
export default (ctx) => {
  const log = (type, input) => appendFileSync(ctx.workspace + '/lifecycle.jsonl', JSON.stringify({type, ...input}) + '\n');
  return {
    apiVersion: 1, name: 'fixture',
    tools: { greet: {
      description: 'Greeting', parameters: {type:'object',properties:{name:{type:'string'}},required:['name'],additionalProperties:false},
      execute(args) { return 'hi ' + args.name; }
    }},
    hooks: {
      'session.start'(input) { log('session.start', input); },
      'session.end'(input) { log('session.end', input); },
      'chat.turn.end'(input) { log('chat.turn.end', input); },
      'session.compacted'(input) { log('session.compacted', input); },
      'chat.turn.failed'(input) { log('chat.turn.failed', input); },
      config(input) { writeFileSync(ctx.workspace + '/config.json', JSON.stringify(input)); },
      'chat.params'(input) { return {temperature: 0.2, topP: 0.9, maxOutputTokens: 1234, reasoningEffort: 'low', options: {seed: 7, echo: input.model}}; },
      'chat.headers'(input) { return {headers: {'x-fixture': 'yes', 'x-model': input.model}}; },
      'permission.ask'(input) {
        if (input.permission === 'bash') return {status: 'deny', reason: 'DENIED_BY_PERMISSION_ASK'};
        if (input.permission === 'read') return input.patterns.some((p) => p.endsWith('.env')) && input.detail.path === '.env' ? {status: 'deny', reason: 'SECRET_READ'} : {status: 'allow'};
      },
      'command.execute.before'(input) { return input.command === 'forbidden' ? {block: 'COMMAND_BLOCKED'} : {context: 'COMMAND_CONTEXT ' + input.arguments}; },
      'shell.env'(input) { return {env: {FIXTURE_ENV: 'from-extension', FIXTURE_CWD: input.cwd}}; },
      'tool.definition'(input) { if (input.toolID === 'ext__fixture__greet') return {description: 'REWRITTEN', parameters: {...input.parameters, description: 'rewritten schema'}}; },
      event(input) { appendFileSync(ctx.workspace + '/events.jsonl', input.type + '\n'); }
    }
  };
};
";

#[tokio::test]
async fn lifecycle_params_env_and_permission_hooks_reach_extensions() {
    let (root, config) = fixture(LIFECYCLE);
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let tools = extensions
        .wrap_tools(Arc::new(EmptyToolRegistry))
        .await
        .unwrap();
    let listed = tools.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].description, "REWRITTEN");
    assert_eq!(listed[0].input_schema["description"], "rewritten schema");
    let tool = tools.get("ext__fixture__greet").unwrap();
    assert_eq!(tool.definition().description, "REWRITTEN");
    assert!(
        tool.execute(json!({"name":1}), context(root.path()))
            .await
            .is_err(),
        "the tool's own contract still validates arguments"
    );
    assert_eq!(
        tool.execute(json!({"name":"you"}), context(root.path()))
            .await
            .unwrap()
            .text,
        "hi you"
    );

    let hooks = extensions.wrap_hooks(None);
    let thread = ThreadId::new();
    let turn = TurnId::new();
    hooks.session_start(thread, "build").await;
    hooks
        .turn_end(
            thread,
            turn,
            &json!({"agent":"build","finishReason":"stop"}),
        )
        .await;
    hooks.session_end(thread, "archived").await;
    let compaction: haarness_protocol::Item = serde_json::from_value(json!({
        "id":haarness_protocol::ItemId::new(), "threadId":thread, "turnId":null,
        "sequence":1, "createdAt":"2026-09-30T00:00:00Z",
        "data":{"type":"compaction","summary":"saved summary","files_touched":[],"outstanding":[]}
    }))
    .unwrap();
    hooks.post_compact(thread, "automatic", &compaction).await;
    hooks
        .stop_failure(thread, turn, &json!({"error":"incomplete"}))
        .await;
    extensions
        .notify_config(&json!({"model":"echo/echo-1"}))
        .await;
    let lifecycle: Vec<Value> = std::fs::read_to_string(root.path().join("lifecycle.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lifecycle[0]["type"], "session.start");
    assert_eq!(lifecycle[0]["agent"], "build");
    assert_eq!(lifecycle[0]["threadId"], json!(thread));
    assert_eq!(lifecycle[0]["directory"], json!(root.path()));
    assert_eq!(lifecycle[1]["type"], "chat.turn.end");
    assert_eq!(lifecycle[1]["turnId"], json!(turn));
    assert_eq!(lifecycle[1]["finishReason"], "stop");
    assert_eq!(lifecycle[2]["type"], "session.end");
    assert_eq!(lifecycle[2]["reason"], "archived");
    assert_eq!(lifecycle[3]["type"], "session.compacted");
    assert_eq!(lifecycle[3]["trigger"], "automatic");
    assert_eq!(lifecycle[3]["compaction"]["id"], json!(compaction.id));
    assert_eq!(lifecycle[4]["type"], "chat.turn.failed");
    assert_eq!(lifecycle[4]["detail"]["error"], "incomplete");
    let delivered: Value =
        serde_json::from_str(&std::fs::read_to_string(root.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(delivered["model"], "echo/echo-1");
    assert_eq!(delivered["workspace"], json!(root.path()));

    let mut request = ProviderRequest {
        model: "fixture-model".into(),
        system: "BASE".into(),
        messages: vec![],
        tools: vec![],
        reasoning_effort: Some("high".into()),
        parameters: haarness_core::RequestParameters::default(),
        output_schema: None,
    };
    hooks.prepare_request(thread, &mut request).await.unwrap();
    assert_eq!(request.parameters.temperature, Some(0.2));
    assert_eq!(request.parameters.top_p, Some(0.9));
    assert_eq!(request.parameters.max_output_tokens, Some(1234));
    assert_eq!(request.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(request.parameters.options["seed"], 7);
    assert_eq!(request.parameters.options["echo"], "fixture-model");
    assert_eq!(request.parameters.headers["x-fixture"], "yes");
    assert_eq!(request.parameters.headers["x-model"], "fixture-model");

    let denied = hooks
        .permission_request(
            thread,
            "bash",
            &["rm -rf /".to_owned()],
            None,
            &json!({"command":"rm -rf /"}),
        )
        .await;
    assert_eq!(denied.permission, Some(PermissionDecision::Deny));
    assert_eq!(denied.block.as_deref(), Some("DENIED_BY_PERMISSION_ASK"));
    let allowed = hooks
        .permission_request(
            thread,
            "read",
            &["x".to_owned()],
            None,
            &json!({"path":"x"}),
        )
        .await;
    assert_eq!(allowed.permission, Some(PermissionDecision::Allow));
    assert!(allowed.block.is_none());
    // The patterns asked about reach the extension beside the arguments.
    let secret = hooks
        .permission_request(
            thread,
            "read",
            &[".env".to_owned()],
            None,
            &json!({"path":".env"}),
        )
        .await;
    assert_eq!(secret.permission, Some(PermissionDecision::Deny));
    assert_eq!(secret.block.as_deref(), Some("SECRET_READ"));
    let open = hooks
        .permission_request(
            thread,
            "write",
            &["x".to_owned()],
            None,
            &json!({"path":"x"}),
        )
        .await;
    assert_eq!(open.permission, None);

    let blocked = hooks.command_execute_before(thread, "forbidden", "").await;
    assert_eq!(blocked.block.as_deref(), Some("COMMAND_BLOCKED"));
    let observed = hooks.command_execute_before(thread, "review", "src").await;
    assert!(observed.block.is_none());
    assert_eq!(observed.context.as_deref(), Some("COMMAND_CONTEXT src"));

    let env = hooks.shell_env(thread, root.path()).await;
    assert_eq!(env["FIXTURE_ENV"], "from-extension");
    assert_eq!(env["FIXTURE_CWD"], root.path().to_string_lossy());

    let events = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
    for expected in [
        "tool.definition",
        "session.start",
        "chat.turn.end",
        "session.compacted",
        "chat.turn.failed",
        "session.end",
        "config",
        "chat.params",
        "chat.headers",
        "permission.ask",
        "command.execute.before",
        "shell.env",
    ] {
        assert!(
            events.lines().any(|line| line == expected),
            "{expected} missing from {events}"
        );
    }
}

#[tokio::test]
async fn unsupported_hooks_and_malformed_results_are_rejected() {
    let (root, config) =
        fixture("export default {apiVersion:1,name:'fixture',hooks:{'bogus.hook'(){}}}");
    assert!(
        Extensions::load(&config, root.path())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("Unsupported hook: bogus.hook")
    );
    let (root, config) = fixture(
        "export default {apiVersion:1,name:'fixture',hooks:{'chat.params'(){return {temperature:'hot'}}, 'permission.ask'(){return {status:'maybe'}}, 'shell.env'(){return {env:{A:1}}}}}",
    );
    let extensions = Extensions::load(&config, root.path()).await.unwrap();
    let hooks = extensions.wrap_hooks(None);
    let thread = ThreadId::new();
    let mut request = ProviderRequest {
        model: "fixture-model".into(),
        system: "BASE".into(),
        messages: vec![],
        tools: vec![],
        reasoning_effort: None,
        parameters: haarness_core::RequestParameters::default(),
        output_schema: None,
    };
    assert!(
        hooks
            .prepare_request(thread, &mut request)
            .await
            .unwrap_err()
            .to_string()
            .contains("temperature must be a number")
    );
    let decision = hooks
        .permission_request(thread, "bash", &[], None, &json!({}))
        .await;
    assert_eq!(decision.permission, Some(PermissionDecision::Deny));
    assert!(decision.block.unwrap().contains("status must be"));
    // A malformed shell.env answer is logged and ignored rather than failing the tool.
    assert!(hooks.shell_env(thread, root.path()).await.is_empty());
}

#[test]
fn entries_accept_bare_sources_and_spec_options_tuples() {
    let config: Config = serde_json::from_value(json!({
        "enabled": true,
        "entries": [
            "/plain.ts",
            ["/tuple.ts", {"greeting": "hi"}],
            {"source": "/object.ts", "enabled": false, "options": {"k": 1}}
        ]
    }))
    .unwrap();
    assert_eq!(config.entries.len(), 3);
    assert_eq!(config.entries[0].source, PathBuf::from("/plain.ts"));
    assert!(config.entries[0].enabled);
    assert_eq!(config.entries[1].source, PathBuf::from("/tuple.ts"));
    assert_eq!(config.entries[1].options, json!({"greeting":"hi"}));
    assert!(!config.entries[2].enabled);
    let written = serde_json::to_value(&config).unwrap();
    assert_eq!(written["entries"][1]["source"], "/tuple.ts");
    assert_eq!(written["entries"][1]["options"]["greeting"], "hi");
    assert!(
        serde_json::from_value::<Config>(json!({"entries":[{"source":"/x.ts","bogus":true}]}))
            .is_err()
    );
    assert!(serde_json::from_value::<Config>(json!({"entries":[["/x.ts"]]})).is_err());
}

/// A redefined browser tool keeps every permission concern of the real one,
/// so an extension cannot turn a Chrome site prompt into one Auto answers.
#[test]
fn redefined_tools_keep_explicit_approvals_and_constraints() {
    struct Site;
    #[async_trait]
    impl Tool for Site {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "browser_navigate".into(),
                description: "Navigate".into(),
                input_schema: json!({"type":"object"}),
            }
        }
        fn side_effectful(&self) -> bool {
            true
        }
        fn permission_constraint(
            &self,
            _arguments: &Value,
            operation: &str,
        ) -> Option<PermissionDecision> {
            operation
                .starts_with("browser:")
                .then_some(PermissionDecision::Deny)
        }
        fn explicit_approval(&self, _arguments: &Value, operation: &str) -> bool {
            operation.starts_with("browser_site:chrome-")
        }
        async fn execute(
            &self,
            _arguments: Value,
            _context: ToolContext,
        ) -> CoreResult<ToolOutput> {
            unreachable!("permission checks only")
        }
    }
    let tool = RedefinedTool {
        inner: Arc::new(Site),
        definition: ToolDefinition {
            name: "browser_navigate".into(),
            description: "Rewritten by an extension".into(),
            input_schema: json!({"type":"object"}),
        },
    };
    let site =
        "browser_site:chrome-browsing:019a0000-0000-7000-8000-000000000000:https://example.com";
    assert!(tool.explicit_approval(&json!({}), site));
    assert!(!tool.explicit_approval(&json!({}), "browser_site:browsing:https://example.com"));
    assert_eq!(
        tool.permission_constraint(&json!({}), "browser:navigate:https://example.com"),
        Some(PermissionDecision::Deny)
    );
    assert_eq!(tool.definition().description, "Rewritten by an extension");
}
