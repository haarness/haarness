#!/usr/bin/env python3
"""Default-on project configuration through CLI, daemon, SDKs and real tmux.

Uses isolated Git projects and an owned stdio MCP fixture. --live also verifies
a real model invokes the project's skill and MCP using persisted tool records.
"""
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error
import uuid
from pathlib import Path

import e2e_scratch
import e2e_tmux
from e2e_env import clean_env

repo = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve()
root = Path(tempfile.mkdtemp(prefix="haarness-project-config-")).resolve()
project = root / "Project 日本 with spaces"
other = root / "Other project"
state = root / "state"
home = root / "home"
env = clean_env()
env.update(HOME=str(home), HAARNESS_HOME=str(home), HAARNESS_DATA_DIR=str(state),
           HAARNESS_TEST_PROVIDER="1", HAARNESS_SCHEDULE_SKIP_INSTALL="1")
marker = "MCP-" + uuid.uuid4().hex
checks = []
connection = {}
sessions = []


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)


def run(*args, **kwargs):
    result = subprocess.run([str(arg) for arg in args], text=True, capture_output=True,
                            timeout=kwargs.pop("timeout", 90), **kwargs)
    assert result.returncode == 0, f"Command failed: {result.stdout}\n{result.stderr}"
    return result.stdout


def cli(*args, workspace=project, extra_env=None, input=None, timeout=90):
    return run(binary, "--workspace", workspace, *args, env={**env, **(extra_env or {})},
               input=input, timeout=timeout)


def check(label, condition=True):
    assert condition, label
    checks.append(label)
    write(root / "checks.json", json.dumps(checks, indent=2))
    print("PASS", label, flush=True)


def until(predicate, label, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.15)
    raise AssertionError(label)


def connect(*args, extra_env=None, workspace=project):
    return json.loads(cli(*args, "desktop", workspace=workspace, extra_env=extra_env,
                          input='{"action":"connect"}'))


def api(path, body=None, target=None):
    endpoint = target or connection
    request = urllib.request.Request(endpoint["url"] + "/v1" + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + endpoint["token"], "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=40) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raise AssertionError(f"{path}: {error.code} {error.read().decode()}") from error


def turn(text, provider="echo", model="echo-1", approve=True):
    thread = api("/threads", {"title": "Project configuration proof", "provider": provider, "model": model})
    value = api(f'/threads/{thread["id"]}/turns', {"requestId": str(uuid.uuid4()), "text": text})
    turn_id = value["id"]
    def done():
        for permission in api("/permissions"):
            assert approve, "Unexpected automatic tool permission bypass"
            api(f'/permissions/{permission["permissionId"]}', {"decision": "once"})
        return api(f"/turns/{turn_id}")["status"] not in ("running", "queued")
    until(done, "Turn did not finish", 180)
    result = api(f"/turns/{turn_id}")
    assert result["status"] == "completed", result
    return api(f'/threads/{thread["id"]}/items')


def names(path, target=None):
    return {item["name"] for item in api(path, target=target)}


def tool_results(items):
    return [item["data"] for item in items if item["data"]["type"] == "tool_result"]


for folder in [project, other, home, state / "config"]:
    folder.mkdir(parents=True, exist_ok=True)
for folder in [project, other]:
    run("git", "init", "-b", "main", folder)
    write(folder / "README.md", "Disposable project configuration fixture.\n")
    run("git", "-C", folder, "add", ".")
    run("git", "-C", folder, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.test",
        "commit", "-m", "Initial fixture")
write(state / "config/config.toml", '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n')
write(state / "config/commands/global-proof.md", "GLOBAL_COMMAND")
write(state / "config/AGENTS.md", "GLOBAL_INSTRUCTIONS")
browser_policy = {"enabled": True, "defaults": {"browsing":"allow", "downloads":"allow", "uploads":"allow"}, "sites":[]}
write(state / "config/haarness.json", json.dumps({"input": {"delivery": "steer"},
                                               "browser": {"policy": {**browser_policy, "enabled": False}}}))
write(project / "AGENTS.md", "PROJECT_INSTRUCTIONS")
write(home / ".claude/settings.json", json.dumps({"model":"echo/claude-home", "enabledPlugins":{"claude-probe@official":True}}))
write(home / ".claude/commands/claude-home.md", "CLAUDE_HOME_COMMAND")
write(home / ".claude/skills/claude-home/SKILL.md", "---\nname: claude-home\ndescription: Claude fixture\n---\nCLAUDE_HOME_SKILL")
claude_plugin = home / ".claude/plugins/cache/claude-probe/1.0.0"
write(claude_plugin / ".claude-plugin/plugin.json", json.dumps({"name":"claude-probe"}))
write(home / ".claude/plugins/installed_plugins.json", json.dumps({"plugins":{"claude-probe@official":[{"scope":"user","installPath":str(claude_plugin),"version":"1.0.0"}]}}))
write(claude_plugin / "commands/claude-plugin.md", "CLAUDE_PLUGIN_COMMAND")
write(project / ".claude/settings.json", json.dumps({"model":"echo/claude-project", "hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":f"touch {root / 'claude-hook-ran'}"}]}]}}))
write(project / ".claude/commands/claude-project.md", "CLAUDE_PROJECT_COMMAND")
write(project / ".claude/skills/claude-project/SKILL.md", "---\nname: claude-project\ndescription: Claude fixture\n---\nCLAUDE_PROJECT_SKILL")
write(project / ".claude/agents/claude-agent.md", "---\ndescription: Claude fixture\n---\nCLAUDE_AGENT")
write(project / "CLAUDE.md", "CLAUDE_INSTRUCTIONS")
write(project / ".mcp.json", json.dumps({"mcpServers":{"claude-mcp":{"command":"false"}}}))
write(project / ".haarness/commands/project-proof.md", "PROJECT_COMMAND $ARGUMENTS")
write(project / ".haarness/agents/project-reader.md", "---\ndescription: Project reader\n---\nRead this project.")
write(project / ".haarness/skills/project-guide/SKILL.md", "---\nname: project-guide\ndescription: Read the project marker using its MCP tool\n---\nCall mcp__fixture__read_marker and report its exact result.")
mcp = root / "mcp.py"
write(mcp, '''import json, sys
from pathlib import Path
Path(sys.argv[2]).write_text("started")
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request: continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"read_marker","description":"Read the project's unique marker","inputSchema":{"type":"object","properties":{}}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":sys.argv[1]}]}
    else: result = {}
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
''')
config = {"mcp": {
    "fixture": {"type": "local", "command": [sys.executable, str(mcp), marker, str(root / "mcp-started")]},
    "disabled_fixture": {"type": "local", "enabled": False,
        "command": [sys.executable, str(mcp), "NEVER", str(root / "disabled-mcp-started")]},
}, "input": {"delivery": "queue"}, "browser": {"policy": browser_policy},
    # A project endpoint with settings the edit form does not show.
    "providers": {"team-proxy": {"protocol": "openai-chat-completions", "name": "Team proxy", "base_url": "https://proxy.fixture.test/v1",
                                 "models": ["m"], "context_window": 123456, "tools": False, "x-team-note": "keep"}}}
hook = shlex.join([sys.executable, "-c", f"from pathlib import Path; Path({str(root / 'hook-ran')!r}).write_text('hook proof')"])
config["hooks"] = {"PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": hook}]}]}
write(project / ".haarness/haarness.json", json.dumps(config))
write(project / ".haarness/extensions.json", '{"enabled":true,"entries":[]}')
write(project / ".haarness/extensions/probe.ts", "export default {apiVersion:1,name:'project_probe',tools:{ping:{description:'Return project extension proof',parameters:{type:'object',properties:{}},execute(){return 'PROJECT_EXTENSION'}}}};")

try:
    connection = connect()
    check("A project opens with configuration enabled and no trust flag", api("/info")["projectTrusted"] is True)
    check("Project commands and global commands load together", {"project-proof", "global-proof"} <= names("/commands"))
    check("Project skills load automatically", "project-guide" in names("/skills"))
    check("Project agent is discovered", "project-reader" in cli("agents", "--json"))
    check("Claude Code commands and skills are absent", not ({"claude-home", "claude-project", "claude-plugin"} & names("/commands")) and not ({"claude-home", "claude-project"} & names("/skills")))
    check("Claude Code agents, plugins, and MCP are absent", "claude-agent" not in cli("agents", "--json") and "claude-probe" not in names("/plugins") and "claude-mcp" not in names("/mcp"))
    write(project / ".claude/commands/added-after-start.md", "LATE_CLAUDE_COMMAND")
    write(project / ".claude/settings.local.json", "invalid Claude JSON")
    time.sleep(1.5)
    check("Claude Code edits cannot enter hot reload", "added-after-start" not in names("/commands") and "project-proof" in names("/commands"))
    check("Project MCP starts; explicitly disabled MCP stays stopped", (root / "mcp-started").exists() and not (root / "disabled-mcp-started").exists())
    check("Project input and browser settings override global", api("/manage/input-settings", {"action": "get"})["data"]["delivery"] == "queue" and api("/manage/browser-settings", {"action": "get"})["data"]["policy"]["enabled"])
    items = turn('!tool mcp__fixture__read_marker {}')
    check("MCP execution is persisted as a real tool result", marker in json.dumps(tool_results(items)))
    check("Project hook actually executes", (root / "hook-ran").exists())
    check("Claude Code hook never executes", not (root / "claude-hook-ran").exists())
    items = turn('!tool skill {"name":"project-guide"}')
    check("Project skill can be invoked", "mcp__fixture__read_marker" in json.dumps(tool_results(items)))
    items = turn('!tool ext__project_probe__ping {}')
    check("Enabled project extension executes", "PROJECT_EXTENSION" in json.dumps(tool_results(items)))
    check("CLI expands a project slash command without an opt-in flag", "PROJECT_COMMAND CLI" in cli("run", "CLI", "--command", "project-proof", "--provider", "echo", "--model", "echo-1"))
    # Editing the endpoint in Settings (the form Desktop and the TUI submit) writes only what it changed.
    view = api("/manage/providers", {"action": "inspect", "name": "team-proxy"})
    edit = next(entry["request"] for entry in view["entries"] if entry["request"].get("action") == "edit_form")
    form = api("/manage/providers", edit)
    values = {field["key"]: field["value"] for field in form["fields"]}
    api("/manage/providers", {**form["submit"], **values, "label": "Proxy for the team", "models": "m, n"})
    saved = json.loads((project / ".haarness/haarness.json").read_text())["providers"]["team-proxy"]
    check("A provider edit writes only the name and models it changed, keeping settings the form does not show",
          saved == {"protocol": "openai-chat-completions", "name": "Proxy for the team", "base_url": "https://proxy.fixture.test/v1",
                    "models": ["m", "n"], "context_window": 123456, "tools": False, "x-team-note": "keep"})
    sibling = connect(workspace=other)
    check("Sibling project remains isolated", "project-guide" not in names("/skills", sibling) and "project-proof" not in names("/commands", sibling))

    # A changed launch policy must not silently reuse a daemon with the opposite setting.
    instance = api("/info")["serverInstanceId"]
    connection = connect("--disable-project-config")
    check("Explicit opt-out replaces an idle daemon with the requested policy", api("/info")["projectTrusted"] is False and api("/info")["serverInstanceId"] != instance)
    check("Opt-out removes project resources while preserving global resources", "project-proof" not in names("/commands") and "project-guide" not in names("/skills") and "global-proof" in names("/commands"))
    check("Opt-out preserves global preference values", api("/manage/input-settings", {"action":"get"})["data"]["delivery"] == "steer" and not api("/manage/browser-settings", {"action":"get"})["data"]["policy"]["enabled"])
    check("Opt-out removes project MCP entries", "fixture" not in names("/mcp"))
    write(project / ".haarness/haarness.json", "MALFORMED PROJECT CONFIG")
    check("Disabled project JSON is ignored even if malformed", "global-proof" in cli("command", "list", extra_env={"HAARNESS_DISABLE_PROJECT_CONFIG":"1"}))
    write(project / ".haarness/haarness.json", json.dumps(config))
    connection = connect(extra_env={"HAARNESS_DISABLE_PROJECT_CONFIG":"true"})
    check("Environment opt-out reaches the daemon", api("/info")["projectTrusted"] is False)
    connection = connect(extra_env={"HAARNESS_DISABLE_PROJECT_CONFIG":"0"})
    check("Returning to the default restores project configuration", api("/info")["projectTrusted"] is True and "project-guide" in names("/skills"))
    check("Legacy trust flag remains compatible", "project-guide" in cli("--trust-project", "skill", "list"))

    schedule_args = ("schedule", "add", "--prompt", "fixture", "--every", "1h", "--model", "echo/echo-1")
    enabled_schedule = cli(*schedule_args, "--name", "policy proof").split()[0]
    check("New schedules inherit the default-on project policy", json.loads(cli("schedule", "get", enabled_schedule))["trustProject"] is True)
    # Schedule names are unique: they are how commands refer to schedules.
    disabled_schedule = cli("--disable-project-config", *schedule_args, "--name", "policy opt-out").split()[0]
    cli("schedule", "update", disabled_schedule, "--name", "policy retained")
    check("Schedule updates retain an explicit opt-out", json.loads(cli("schedule", "get", disabled_schedule))["trustProject"] is False)
    cli("--trust-project", "schedule", "update", disabled_schedule, "--name", "policy enabled")
    check("An explicit schedule policy update takes effect", json.loads(cli("schedule", "get", disabled_schedule))["trustProject"] is True)
    cli("schedule", "remove", enabled_schedule)
    cli("schedule", "remove", disabled_schedule)

    # An actual permission request proves default-on configuration does not set yolo.
    thread = api("/threads", {"title":"Permission proof", "provider":"echo", "model":"echo-1"})
    pending = api(f'/threads/{thread["id"]}/turns', {"requestId":str(uuid.uuid4()), "text":'!tool bash {"command":"printf permission-proof"}'})
    until(lambda: api("/permissions"), "Normal bash permission was bypassed")
    failed = subprocess.run([str(binary), "--workspace", str(project), "--disable-project-config", "desktop"],
        input='{"action":"connect"}', text=True, env=env, capture_output=True, timeout=30)
    check("Active work blocks a policy-changing restart", failed.returncode != 0 and "active turn" in failed.stderr)
    api(f'/permissions/{api("/permissions")[0]["permissionId"]}', {"decision":"once"})
    until(lambda: api(f'/turns/{pending["id"]}')["status"] == "completed", "Approved turn did not complete")

    for attached in [False, True]:
        session = "haarness-project-config-" + uuid.uuid4().hex[:10]
        sessions.append(session)
        args = [str(binary), "--workspace", str(project)]
        if attached: args += ["attach", connection["url"], "--token", connection["token"]]
        launch = e2e_tmux.pane_command(env, args)
        run("tmux", "new-session", "-d", "-x", "112", "-y", "34", "-s", session, launch)
        def pane(): return run("tmux", "capture-pane", "-p", "-t", session)
        until(lambda: "/ commands" in pane(), "TUI did not open")
        prompt = 'TUI project configuration ' + uuid.uuid4().hex[:8]
        run("tmux", "send-keys", "-l", "-t", session, prompt)
        run("tmux", "send-keys", "-t", session, "Enter")
        until(lambda: any(prompt in item["data"].get("text", "") and item["data"]["type"] == "assistant_message"
                          for t in api("/threads") for item in api(f'/threads/{t["id"]}/items')), "TUI reply was not persisted")
        write(root / ("attached-tui.txt" if attached else "local-tui.txt"), pane())
        check(("Attached" if attached else "Local") + " TUI replies with default project configuration")
        run("tmux", "kill-session", "-t", session)
        sessions.remove(session)

    if "--live" in sys.argv:
        key = os.environ.get("OPENAI_API_KEY")
        if not key:
            key = json.loads((Path.home() / ".local/share/haarness/auth.json").read_text())["openai"]["key"]
        # Credentials go only to the isolated child, never to fixtures or output.
        cli("serve", "--stop")
        connection = connect(extra_env={"OPENAI_API_KEY": key})
        items = turn("Load the project-guide skill with the skill tool, then follow it to call mcp__fixture__read_marker. Report the exact marker returned. Use both tools.", "openai", "gpt-5.4-mini")
        write(root / "live-items.json", json.dumps(items, indent=2))
        calls = [item["data"].get("name", "") for item in items if item["data"]["type"] == "tool_call"]
        check("Real model invokes the project skill and MCP", "skill" in calls and "mcp__fixture__read_marker" in calls and marker in json.dumps(tool_results(items)))
finally:
    for session in sessions:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    for folder in [project, other]:
        try: cli("serve", "--stop", workspace=folder)
        except Exception: pass
    e2e_scratch.settle(root)
