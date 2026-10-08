use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, time::timeout};
use uuid::Uuid;

const TOKEN: &str = "hot-reload-e2e-token";

#[tokio::test]
async fn capability_management_disables_execution_and_survives_restart() {
    let root=TestDirectory::new();
    let root_path=root.0.canonicalize().unwrap();
    let workspace=root_path.join("workspace");let data=root_path.join("data");let home=root_path.join("home");
    for path in [&workspace,&data,&home] {std::fs::create_dir_all(path).unwrap();}
    let address=unused_address().await;
    let mut process=server_command(&workspace,&data,&home,address).spawn().unwrap();
    wait_until_listening(address).await;
    let client=Client::new();
    let manage=|input: Value| client.post(format!("http://{address}/v1/manage/capabilities")).bearer_auth(TOKEN).json(&input).send();
    for input in [json!({"action":"create","kind":"skill","name":"live-proof","scope":"local","description":"Proof","instructions":"Return proof."}),json!({"action":"create","kind":"agent","name":"live-agent","scope":"local","description":"Verifier","prompt":"Verify proof."})] {
        let response=manage(input).await.unwrap();let status=response.status();let body=response.text().await.unwrap();assert!(status.is_success(),"{body}");
    }
    wait_for_array_entry(&client,address,"/skills","name","live-proof",true).await;
    wait_for_array_entry(&client,address,"/agents","id","live-agent",true).await;
    wait_for_array_entry(&client,address,"/commands","name","live-proof",true).await;
    for (kind,name) in [("skill","live-proof"),("agent","live-agent")] {
        manage(json!({"action":"disable","kind":kind,"name":name,"scope":"local"})).await.unwrap().error_for_status().unwrap();
    }
    wait_for_array_entry(&client,address,"/skills","name","live-proof",false).await;
    wait_for_array_entry(&client,address,"/agents","id","live-agent",false).await;
    wait_for_array_entry(&client,address,"/commands","name","live-proof",false).await;
    let response=client.get(format!("http://{address}/v1/skills/live-proof")).bearer_auth(TOKEN).send().await.unwrap();
    assert!(!response.status().is_success(),"Disabled skills must reject direct loading");
    assert!(workspace.join(".haarness/skills/live-proof/SKILL.md").exists());
    let catalog: Value=manage(json!({"action":"list"})).await.unwrap().json().await.unwrap();
    assert!(catalog["skills"].as_array().unwrap().iter().any(|s|s["name"]=="live-proof" && s["enabled"]==false));
    process.kill().await.unwrap();process.wait().await.unwrap();
    let mut process=server_command(&workspace,&data,&home,address).spawn().unwrap();
    wait_until_listening(address).await;
    assert!(!array_has(&get_json(&client,address,"/skills").await,"name","live-proof"));
    assert!(!array_has(&get_json(&client,address,"/agents").await,"id","live-agent"));
    for (kind,name) in [("skill","live-proof"),("agent","live-agent")] {
        manage(json!({"action":"enable","kind":kind,"name":name,"scope":"local"})).await.unwrap().error_for_status().unwrap();
    }
    wait_for_array_entry(&client,address,"/skills","name","live-proof",true).await;
    wait_for_array_entry(&client,address,"/agents","id","live-agent",true).await;
    process.kill().await.unwrap();process.wait().await.unwrap();
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-hot-reload-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("create isolated test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn unused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    listener.local_addr().expect("ephemeral address")
}

async fn wait_until_listening(address: SocketAddr) {
    for _ in 0..300 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Haarness did not listen on {address}");
}

fn server_command(workspace: &Path, data: &Path, home: &Path, address: SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
    command
        .arg("--workspace")
        .arg(workspace)
        .arg("--data-dir")
        .arg(data)
        .arg("--trust-project")
        .arg("serve")
        .arg("--host")
        .arg(address.ip().to_string())
        .arg("--port")
        .arg(address.port().to_string())
        .arg("--token")
        .arg(TOKEN)
        .env("HOME", home)
        .env("HAARNESS_TEST_PROVIDER", "1")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn get_json(client: &Client, address: SocketAddr, path: &str) -> Value {
    client
        .get(format!("http://{address}/v1{path}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("send API request")
        .error_for_status()
        .expect("successful API response")
        .json()
        .await
        .expect("JSON API response")
}

fn array_has(value: &Value, field: &str, expected: &str) -> bool {
    value.as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry[field].as_str() == Some(expected))
    })
}

async fn wait_for_array_entry(
    client: &Client,
    address: SocketAddr,
    path: &str,
    field: &str,
    expected: &str,
    present: bool,
) -> Value {
    let mut latest = Value::Null;
    for _ in 0..120 {
        latest = get_json(client, address, path).await;
        if array_has(&latest, field, expected) == present {
            return latest;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{path} did not reach {field}={expected:?}, present={present}; latest={latest}");
}

async fn wait_for_command_description(
    client: &Client,
    address: SocketAddr,
    name: &str,
    description: &str,
) {
    for _ in 0..120 {
        let commands = get_json(client, address, "/commands").await;
        if commands.as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["name"].as_str() == Some(name)
                    && entry["description"].as_str() == Some(description)
            })
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("command {name} did not reach description {description:?}");
}

async fn wait_until_turn_idle(client: &Client, address: SocketAddr, thread_id: &str) {
    for _ in 0..120 {
        if get_json(client, address, &format!("/threads/{thread_id}/active")).await == Value::Null {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("turn {thread_id} did not become idle");
}

async fn wait_for_file_contents(path: &Path, expected: &str) {
    for _ in 0..120 {
        if std::fs::read_to_string(path).is_ok_and(|contents| contents == expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "{} did not reach {expected:?}; latest={:?}",
        path.display(),
        std::fs::read_to_string(path).ok()
    );
}

fn plugin_manifest(mcp_names: &[&str]) -> String {
    let servers = mcp_names
        .iter()
        .map(|name| ((*name).to_owned(), json!({"command":"sh", "disabled":true})))
        .collect::<serde_json::Map<_, _>>();
    serde_json::to_string_pretty(&json!({
        "name":"reload-fixture",
        "description":"hot reload fixture",
        "mcpServers":servers,
    }))
    .expect("serialize plugin manifest")
}

fn plugin_hook(marker: &Path, generation: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "hooks": {
            "UserPromptSubmit": [{
                "hooks": [{
                    "command": "sh",
                    "args": ["-c", format!(
                        "printf '%s' '{}' > '{}'",
                        generation,
                        marker.display()
                    )]
                }]
            }]
        }
    }))
    .expect("serialize plugin hook")
}

fn provider_config(ids: &[&str]) -> String {
    ids.iter()
        .map(|id| {
            format!(
                "[providers.{id}]\nbase_url = \"http://127.0.0.1:9/v1\"\nrequires_auth = false\nmodels = [\"{id}-model\"]\n"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reloads_runtime_assets_atomically_and_defers_while_a_turn_is_active() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create project config root");
    std::fs::create_dir_all(&home).expect("create isolated home");
    let config = workspace.join(".haarness/config.toml");
    let address = unused_address().await;
    let child = server_command(&workspace, &data, &home, address)
        .spawn()
        .expect("launch Haarness server");
    wait_until_listening(address).await;
    let client = Client::new();

    let info = get_json(&client, address, "/info").await;
    assert!(
        info["capabilities"]
            .as_array()
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| entry == "configuration-hot-reload.v1")),
        "server does not advertise hot-reload support: {info}"
    );
    let instance = info["serverInstanceId"]
        .as_str()
        .expect("server instance id")
        .to_owned();

    // Idle add: provider definitions and standalone project skills become
    // visible without replacing the server process or event stream identity.
    std::fs::write(&config, provider_config(&["idle-provider"]))
        .expect("write initial provider config");
    wait_for_array_entry(&client, address, "/providers", "id", "idle-provider", true).await;
    let skill = workspace.join(".haarness/skills/live/SKILL.md");
    std::fs::create_dir_all(skill.parent().expect("skill parent")).expect("create skill root");
    std::fs::write(
        &skill,
        "---\nname: live-skill\ndescription: first generation\n---\nBODY_ONE\n",
    )
    .expect("write skill");
    wait_for_array_entry(&client, address, "/skills", "name", "live-skill", true).await;
    std::fs::write(
        &skill,
        "---\nname: live-skill\ndescription: second generation\n---\nBODY_TWO\n",
    )
    .expect("edit skill");
    for _ in 0..120 {
        let loaded = get_json(&client, address, "/skills/live-skill").await;
        if loaded["instructions"]
            .as_str()
            .is_some_and(|body| body.contains("BODY_TWO"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        get_json(&client, address, "/skills/live-skill").await["instructions"]
            .as_str()
            .is_some_and(|body| body.contains("BODY_TWO")),
        "skill body edit did not hot reload"
    );

    // Adding a trusted local plugin mounts its commands, skills, and workflow
    // catalog as one runtime generation.
    let plugin = workspace.join(".haarness/plugins/reload-fixture");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).expect("create plugin manifest root");
    std::fs::create_dir_all(plugin.join("commands")).expect("create plugin command root");
    std::fs::create_dir_all(plugin.join("skills/reload-skill")).expect("create plugin skill root");
    std::fs::create_dir_all(plugin.join("workflows")).expect("create plugin workflow root");
    std::fs::create_dir_all(plugin.join("hooks")).expect("create plugin hooks root");
    let hook_marker = directory.0.join("hook-generation");
    let hooks = plugin.join("hooks/hooks.json");
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        plugin_manifest(&["fixture-mcp"]),
    )
    .expect("write plugin manifest");
    std::fs::write(&hooks, plugin_hook(&hook_marker, "one")).expect("write plugin hooks");
    let plugin_command = plugin.join("commands/hot.md");
    std::fs::write(
        &plugin_command,
        "---\ndescription: plugin generation one\n---\nPLUGIN_ONE $ARGUMENTS\n",
    )
    .expect("write plugin command");
    std::fs::write(
        plugin.join("skills/reload-skill/SKILL.md"),
        "---\nname: reload-skill\ndescription: plugin skill\n---\nPLUGIN_SKILL\n",
    )
    .expect("write plugin skill");
    std::fs::write(
        plugin.join("workflows/hot.ts"),
        "export default workflow({ name: 'hot-flow', description: 'plugin workflow', run() {} })\n",
    )
    .expect("write plugin workflow");
    wait_for_command_description(
        &client,
        address,
        "reload-fixture:hot",
        "plugin generation one",
    )
    .await;
    wait_for_array_entry(&client, address, "/skills", "name", "reload-fixture:reload-skill", true).await;
    wait_for_array_entry(&client, address, "/mcp", "name", "fixture-mcp", true).await;
    let workflows = get_json(&client, address, "/workflows").await;
    assert!(
        workflows["workflows"].as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["invocation"]
                    .as_str()
                    .is_some_and(|name| name.contains("hot-flow"))
            })
        }),
        "plugin workflow was not mounted: {workflows}"
    );

    // Active-turn edit: the watcher can compose a candidate, but provider and
    // command catalogs must remain on generation one until the turn exits.
    let thread: Value = client
        .post(format!("http://{address}/v1/threads"))
        .bearer_auth(TOKEN)
        .json(&json!({"title":"reload fence","provider":"echo","model":"echo-1","agent":"build"}))
        .send()
        .await
        .expect("create thread")
        .error_for_status()
        .expect("create thread status")
        .json()
        .await
        .expect("create thread JSON");
    let thread_id = thread["id"].as_str().expect("thread id");
    let turn = client
        .post(format!("http://{address}/v1/threads/{thread_id}/turns"))
        .bearer_auth(TOKEN)
        .json(&json!({
            "requestId": Uuid::now_v7(),
            "text": "::mdslow",
            "provider": "echo",
            "model": "echo-1"
        }))
        .send()
        .await
        .expect("start slow turn");
    assert_eq!(turn.status(), StatusCode::ACCEPTED);
    wait_for_file_contents(&hook_marker, "one").await;
    std::fs::write(&config, provider_config(&["idle-provider", "during-turn"]))
        .expect("edit provider during turn");
    std::fs::write(
        &plugin_command,
        "---\ndescription: plugin generation two\n---\nPLUGIN_TWO $ARGUMENTS\n",
    )
    .expect("edit plugin command during turn");
    std::fs::write(&hooks, plugin_hook(&hook_marker, "two")).expect("edit plugin hook during turn");
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        plugin_manifest(&["fixture-mcp", "during-turn-mcp"]),
    )
    .expect("edit plugin MCP definitions during turn");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        !array_has(
            &get_json(&client, address, "/providers").await,
            "id",
            "during-turn"
        ),
        "provider generation leaked into an active turn"
    );
    assert!(
        !array_has(
            &get_json(&client, address, "/mcp").await,
            "name",
            "during-turn-mcp"
        ),
        "plugin MCP generation leaked into an active turn"
    );
    let commands = get_json(&client, address, "/commands").await;
    assert!(
        commands
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| {
                entry["name"].as_str() == Some("reload-fixture:hot")
                    && entry["description"].as_str() == Some("plugin generation one")
            }))
    );
    wait_until_turn_idle(&client, address, thread_id).await;
    wait_for_array_entry(&client, address, "/providers", "id", "during-turn", true).await;
    wait_for_array_entry(&client, address, "/mcp", "name", "during-turn-mcp", true).await;
    wait_for_command_description(
        &client,
        address,
        "reload-fixture:hot",
        "plugin generation two",
    )
    .await;
    let hook_turn = client
        .post(format!("http://{address}/v1/threads/{thread_id}/turns"))
        .bearer_auth(TOKEN)
        .json(&json!({
            "requestId": Uuid::now_v7(),
            "text": "hook generation two",
            "provider": "echo",
            "model": "echo-1"
        }))
        .send()
        .await
        .expect("start post-reload hook turn");
    assert_eq!(hook_turn.status(), StatusCode::ACCEPTED);
    wait_until_turn_idle(&client, address, thread_id).await;
    wait_for_file_contents(&hook_marker, "two").await;

    // Invalid edits preserve every part of the last-known-good generation;
    // a later valid edit recovers automatically and can remove old entries.
    std::fs::write(&config, "[providers.broken\n").expect("write malformed config");
    tokio::time::sleep(Duration::from_millis(900)).await;
    let providers = get_json(&client, address, "/providers").await;
    assert!(array_has(&providers, "id", "idle-provider"));
    assert!(array_has(&providers, "id", "during-turn"));
    std::fs::write(
        &config,
        "[providers.semantic-broken]\nprotocol = \"not-a-protocol\"\nbase_url = \"http://127.0.0.1:9/v1\"\nmodels = [\"broken\"]\n",
    )
    .expect("write semantically invalid provider config");
    tokio::time::sleep(Duration::from_millis(900)).await;
    let providers = get_json(&client, address, "/providers").await;
    assert!(array_has(&providers, "id", "idle-provider"));
    assert!(array_has(&providers, "id", "during-turn"));
    assert!(!array_has(&providers, "id", "semantic-broken"));
    std::fs::write(&config, provider_config(&["recovered-provider"]))
        .expect("repair provider config");
    wait_for_array_entry(
        &client,
        address,
        "/providers",
        "id",
        "recovered-provider",
        true,
    )
    .await;
    wait_for_array_entry(&client, address, "/providers", "id", "idle-provider", false).await;

    std::fs::remove_dir_all(&plugin).expect("remove plugin root");
    wait_for_array_entry(
        &client,
        address,
        "/commands",
        "name",
        "reload-fixture:hot",
        false,
    )
    .await;
    wait_for_array_entry(&client, address, "/skills", "name", "reload-fixture:reload-skill", false).await;
    wait_for_array_entry(&client, address, "/mcp", "name", "fixture-mcp", false).await;
    wait_for_array_entry(&client, address, "/mcp", "name", "during-turn-mcp", false).await;
    let mut workflow_removed = false;
    for _ in 0..120 {
        let workflows = get_json(&client, address, "/workflows").await;
        workflow_removed = workflows["workflows"].as_array().is_some_and(|entries| {
            entries.iter().all(|entry| {
                !entry["invocation"]
                    .as_str()
                    .is_some_and(|name| name.contains("hot-flow"))
            })
        });
        if workflow_removed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(workflow_removed, "plugin workflow did not unmount");
    std::fs::write(&hook_marker, "sentinel").expect("reset hook marker after plugin removal");
    let removed_hook_turn = client
        .post(format!("http://{address}/v1/threads/{thread_id}/turns"))
        .bearer_auth(TOKEN)
        .json(&json!({
            "requestId": Uuid::now_v7(),
            "text": "hook must stay removed",
            "provider": "echo",
            "model": "echo-1"
        }))
        .send()
        .await
        .expect("start post-removal hook turn");
    assert_eq!(removed_hook_turn.status(), StatusCode::ACCEPTED);
    wait_until_turn_idle(&client, address, thread_id).await;
    assert_eq!(
        std::fs::read_to_string(&hook_marker).expect("read post-removal hook marker"),
        "sentinel",
        "removed plugin hook still executed"
    );
    assert_eq!(
        get_json(&client, address, "/info").await["serverInstanceId"],
        instance,
        "hot reload must retain the same server process identity"
    );

    let shutdown = client
        .post(format!("http://{address}/v1/shutdown"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("request shutdown");
    assert_eq!(shutdown.status(), StatusCode::ACCEPTED);
    let output = timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("server shutdown timeout")
        .expect("wait for server");
    assert!(
        output.status.success(),
        "server failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn line_count(path: &Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

async fn wait_for_mcp_status(client: &Client, address: SocketAddr, name: &str, status: &str) {
    let mut latest = Value::Null;
    for _ in 0..200 {
        latest = get_json(client, address, "/mcp").await;
        if latest.as_array().is_some_and(|servers| {
            servers
                .iter()
                .any(|server| server["name"] == name && server["status"] == status)
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("MCP server {name} did not reach {status}; latest={latest}");
}

/// A reload follows content, not file events: rewriting an instruction file
/// unchanged, an editor's scratch files and an OAuth token refresh reload
/// nothing, and a real reload keeps unchanged MCP servers connected rather
/// than respawning their processes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unchanged_sources_neither_reload_nor_respawn_mcp_servers() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    let home = directory.0.join("home");
    std::fs::create_dir_all(workspace.join(".haarness")).expect("create project config root");
    std::fs::create_dir_all(data.join("data")).expect("create isolated data root");
    std::fs::create_dir_all(&home).expect("create isolated home");
    let launches = directory.0.join("launches");
    let reloads = directory.0.join("reloads");
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../haarness-mcp/tests/fixtures/recovery.py"
    );
    std::fs::write(
        workspace.join(".haarness/haarness.json"),
        serde_json::to_string_pretty(&json!({
            "mcp": {"counter": {
                "command": "python3",
                "args": [fixture],
                "env": {"LAUNCH_LOG": launches, "CALL_LOG": directory.0.join("calls")}
            }},
            "hooks": {"ConfigChange": [{"hooks": [{
                "type": "command",
                "command": format!("printf 'reload\\n' >> '{}'", reloads.display())
            }]}]}
        }))
        .expect("serialize project config"),
    )
    .expect("write project config");
    let agents = workspace.join("AGENTS.md");
    std::fs::write(&agents, "Use tabs.\n").expect("write instructions");
    // A provider this build does not know, so no credential reaches a network.
    let auth = data.join("data/auth.json");
    let credential = |access: &str| {
        json!({"fixture-oauth": {"type": "oauth", "access": access, "refresh": access, "expires": 1}})
            .to_string()
    };
    std::fs::write(&auth, credential("first")).expect("write credentials");
    let address = unused_address().await;
    let mut process = server_command(&workspace, &data, &home, address)
        .spawn()
        .expect("launch Haarness server");
    wait_until_listening(address).await;
    let client = Client::new();
    wait_for_mcp_status(&client, address, "counter", "connected").await;
    assert_eq!(line_count(&launches), 1);

    std::fs::write(&agents, "Use tabs.\n").expect("rewrite instructions unchanged");
    for scratch in [".haarness.json.swp", "haarness.json~", "4913"] {
        std::fs::write(workspace.join(".haarness").join(scratch), "scratch")
            .expect("write editor scratch file");
    }
    std::fs::write(&auth, credential("refreshed")).expect("refresh the OAuth token");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        line_count(&reloads),
        0,
        "an unchanged source reloaded the configuration"
    );
    assert_eq!(
        line_count(&launches),
        1,
        "an unchanged source respawned the MCP server"
    );

    std::fs::write(&agents, "Use spaces.\n").expect("edit instructions");
    for _ in 0..200 {
        if line_count(&reloads) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(line_count(&reloads), 1, "an edited source did not reload");
    wait_for_mcp_status(&client, address, "counter", "connected").await;
    assert_eq!(
        line_count(&launches),
        1,
        "the reload respawned an unchanged MCP server"
    );
    let ping = client
        .post(format!("http://{address}/v1/mcp/counter/tools/ping"))
        .bearer_auth(TOKEN)
        .json(&json!({}))
        .send()
        .await
        .expect("call the reused MCP server");
    let status = ping.status();
    let body = ping.text().await.unwrap_or_default();
    assert!(
        status.is_success() && body.contains("pong"),
        "{status}: {body}"
    );
    process.kill().await.expect("stop server");
    process.wait().await.expect("reap server");
}
