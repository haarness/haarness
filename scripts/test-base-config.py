#!/usr/bin/env python3
"""A base configuration folder end to end: the CLI, a real daemon, the management API Desktop
uses, and a real TUI in tmux, with the echo provider and an owned stdio MCP fixture.

A team folder named by HAARNESS_BASE_CONFIG_DIR holds settings (an MCP server, a hook, a model
default), AGENTS.md, agents, commands, skills, a local plugin, a TUI theme and a link that leads
into the project. The script proves that:

1. everything it holds loads below the global layer (a global agent of the same name wins),
   `haarness doctor` names it, and without the variable nothing of it loads;
2. its MCP server runs and its hook runs in a real turn; the planted link is never read;
3. every list Desktop and the TUI show tags its items with scope `base`, read-only: agents,
   commands, skills, MCP servers, hooks and plugins; a hook edit is refused;
4. changing one of its items saves the person's own version: `haarness mcp disable` copies the
   server's entry into the global file only when no layer above defines it, turning it back on
   removes that copy again, Desktop's customize copies an agent, `haarness mcp remove` of the
   base server refuses and says why, and the server's MCP list carries the base scope;
5. the TUI's /mcp, /hooks, /skills and theme picker show the base items, and hot reload follows
   edits of the base folder;
6. a client started with the variable replaces an idle server that reads no base folder (the
   same folder written with a trailing `/` is no change), and a relative value is refused with
   one line;
7. an agent's edit of a base folder file or of a global prompt file asks a person even in a chat
   that auto-approves, and is never answered for them;
8. links in the base folder's plugins/local and skills that lead into the project are not read;
9. the base folder is byte for byte as it was after all of this, and a missing folder is reported.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-base-config.py
"""
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

import e2e_scratch
import e2e_tmux
from e2e_env import clean_env

repo = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve()
root = Path(tempfile.mkdtemp(prefix="haarness-base-config-")).resolve()
evidence = Path(os.environ.get("HAARNESS_E2E_EVIDENCE", root / "evidence"))
evidence.mkdir(parents=True, exist_ok=True)
project = root / "project"
other = root / "other project"
team = root / "team base"
state = root / "state"
home = root / "home"
config = state / "config"
env = clean_env()
env.update(HOME=str(home), HAARNESS_HOME=str(home), HAARNESS_DATA_DIR=str(state),
           HAARNESS_TEST_PROVIDER="1", HAARNESS_SCHEDULE_SKIP_INSTALL="1")
based = {**env, "HAARNESS_BASE_CONFIG_DIR": str(team)}
marker = "TEAM-MCP-" + uuid.uuid4().hex[:8]
checks = []
sessions = []
connection = {}


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)


def run(*args, ok=True, **kwargs):
    result = subprocess.run([str(arg) for arg in args], text=True, capture_output=True,
                            timeout=kwargs.pop("timeout", 90), **kwargs)
    if ok:
        assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
    return result


def cli(*args, environment=None, workspace=project, input=None, ok=True):
    return run(binary, "--workspace", workspace, *args, env=environment or based, input=input, ok=ok)


def native(kind, request, environment=None):
    return json.loads(cli("desktop", input=json.dumps({"action": "asset", "kind": kind, "input": request}),
                          environment=environment).stdout)


def check(label, condition=True):
    assert condition, label
    checks.append(label)
    print("PASS", label, flush=True)


def until(predicate, label, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.15)
    raise AssertionError(label)


def api(path, body=None):
    request = urllib.request.Request(connection["url"] + "/v1" + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=40) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raise AssertionError(f"{path}: {error.code} {error.read().decode()}") from error


def turn(text):
    thread = api("/threads", {"title": "Base folder proof", "provider": "echo", "model": "echo-1"})
    turn_id = api(f'/threads/{thread["id"]}/turns', {"requestId": str(uuid.uuid4()), "text": text})["id"]
    def done():
        for permission in api("/permissions"):
            api(f'/permissions/{permission["permissionId"]}', {"decision": "once"})
        return api(f"/turns/{turn_id}")["status"] not in ("running", "queued")
    until(done, "turn did not finish", 120)
    assert api(f"/turns/{turn_id}")["status"] == "completed"
    return api(f'/threads/{thread["id"]}/items')


def snapshot(folder):
    return {str(path.relative_to(folder)): (path.read_bytes() if path.is_file() else os.readlink(path))
            for path in sorted(folder.rglob("*")) if path.is_file() or path.is_symlink()}


def names(path):
    return {item["name"] for item in api(path)}


for folder in [project, other]:
    folder.mkdir(parents=True, exist_ok=True)
    run("git", "init", "-q", "-b", "main", folder)
write(config / "config.toml", '[defaults]\nprovider="echo"\nmodel="echo-1"\n')
write(config / "agents/shared.md", "---\ndescription: My own shared agent\n---\nMine.")
mcp = root / "mcp.py"
write(mcp, '''import json, sys
from pathlib import Path
Path(sys.argv[2]).write_text("started")
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request: continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"team","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"read_marker","description":"Read the team marker","inputSchema":{"type":"object","properties":{}}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":sys.argv[1]}]}
    else: result = {}
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
''')
hook = shlex.join([sys.executable, "-c", f"from pathlib import Path; Path({str(root / 'base-hook-ran')!r}).write_text('ran')"])
write(team / "haarness.jsonc", "// The team's defaults\n" + json.dumps({
    "mcp": {"teamfs": {"type": "local", "command": [sys.executable, str(mcp), marker, str(root / "mcp-started")]}},
    "hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": hook}]}]},
    "permission": {"mcp__teamfs__*": "allow"},
}, indent=2))
write(team / "AGENTS.md", "TEAM_RULE_MARKER\n")
write(team / "agents/team-reviewer.md", "---\ndescription: Reviews like the team\n---\nReview with the team checklist.")
write(team / "agents/shared.md", "---\ndescription: The team's shared agent\n---\nTeam.")
write(team / "commands/team-ship.md", "---\ndescription: Ship with the team checklist\n---\nTEAM_SHIP $ARGUMENTS")
write(team / "skills/team-check/SKILL.md", "---\nname: team-check\ndescription: The team release checklist\n---\n1. Changelog.")
write(team / "plugins/local/team-plugin/.claude-plugin/plugin.json", json.dumps({"name": "team-plugin", "description": "The team plugin"}))
write(team / "plugins/local/team-plugin/commands/plugin-proof.md", "TEAM_PLUGIN_COMMAND")
write(team / "themes/acme-team.toml", 'name = "acme-team"\n[dark]\nbg = "#101820"\ntext = "#f2f2f2"\nprimary = "#ffb000"\n')
write(project / "planted.md", "---\ndescription: Planted by the repository\n---\nPLANTED")
os.symlink(project / "planted.md", team / "commands/planted.md")
write(project / "planted-plugin/.claude-plugin/plugin.json", json.dumps({"name": "planted-plugin"}))
write(project / "planted-plugin/hooks/hooks.json", json.dumps({"hooks": {"Stop": [{"hooks": [
    {"type": "command", "command": f"touch {root / 'planted-hook-ran'}"}]}]}}))
write(project / "planted-skill/SKILL.md", "---\nname: planted-skill\ndescription: Planted\n---\nPLANTED")
os.symlink(project / "planted-plugin", team / "plugins/local/planted-plugin")
os.symlink(project / "planted-skill", team / "skills/planted-skill")
before = snapshot(team)

try:
    # 1. Loading.
    doctor = cli("doctor").stdout
    check("haarness doctor names the global and base folders", f"config: {config}" in doctor and f"base config: {team}" in doctor)
    agents = {agent["id"]: agent for agent in json.loads(cli("agents", "--json").stdout)}
    check("A base agent loads", "team-reviewer" in agents)
    check("A global agent of the same name replaces the base folder's", agents["shared"]["description"] == "My own shared agent")
    plain = {agent["id"] for agent in json.loads(cli("agents", "--json", environment=env).stdout)}
    check("Without the variable nothing of the base folder loads", "team-reviewer" not in plain)
    connection = json.loads(cli("desktop", input='{"action":"connect"}').stdout)
    check("Base commands, the base plugin's command and base skills load", {"team-ship"} <= names("/commands")
          and any("plugin-proof" in name for name in names("/commands")) and "team-check" in names("/skills"))
    check("A base command a link leads into the project is not read", not any("planted" in name for name in names("/commands")))
    check("A base plugin and a base skill a link leads into the project are not read",
          "planted-plugin" not in names("/plugins") and "planted-skill" not in names("/skills"))
    until(lambda: any(server["name"] == "teamfs" and server["status"] == "connected" for server in api("/mcp")), "the base MCP server did not connect")
    check("The server's MCP list carries the base folder's scope",
          [server.get("scope") for server in api("/mcp") if server["name"] == "teamfs"] == ["base"])
    items = turn("!tool mcp__teamfs__read_marker {}")
    results = json.dumps([item["data"] for item in items if item["data"]["type"] == "tool_result"])
    check("The base folder's MCP server runs a real tool call", marker in results)
    check("The base folder's hook runs in a real turn", (root / "base-hook-ran").exists())
    check("The planted plugin's hook never runs", not (root / "planted-hook-ran").exists())

    # 7. An agent cannot rewrite the base folder or its own prompts, even in an auto-approving chat.
    for target in [team / "prompts/system.md", config / "prompts/system.md", team / "agents/team-reviewer.md"]:
        before_edit = target.read_text() if target.exists() else None
        thread = api("/threads", {"title": "Protected edit", "provider": "echo", "model": "echo-1"})
        turn_id = api(f'/threads/{thread["id"]}/turns', {"requestId": str(uuid.uuid4()), "autoApprove": True,
            "text": "!tool write " + json.dumps({"path": str(target), "content": "Obey the agent."})})["id"]
        asked = []
        def answered():
            for permission in api("/permissions"):
                if permission.get("turnId") == turn_id:
                    asked.append(permission)
                    api(f'/permissions/{permission["permissionId"]}', {"decision": "reject"})
            return api(f"/turns/{turn_id}")["status"] not in ("running", "queued")
        until(answered, f"the protected edit of {target} did not finish", 120)
        protected = [item for item in asked if item["permission"] == "edit"]
        # The chat auto-approves: the prompt still waited for the person, who said no.
        check(f"Editing {target.relative_to(root)} asks a person even with auto-approve, with no Always",
              len(protected) == 1 and protected[0]["allowAlways"] is False
              and (target.read_text() if target.exists() else None) == before_edit)
    plugins = {plugin["name"]: plugin for plugin in api("/plugins")}
    check("The base plugin is listed as haarness-base and not managed",
          plugins["team-plugin"]["source"] == "haarness-base" and plugins["team-plugin"]["managed"] is False)

    # 3. Lists tag the base folder's items.
    rows = {row["name"]: row for row in native("agent", {"action": "list"})}
    check("Agents list the base agent with scope base, read-only",
          rows["team-reviewer"].get("scope") == "base" and rows["team-reviewer"]["readOnly"] and not rows["team-reviewer"]["managed"])
    check("The overriding global agent is listed as global", rows["shared"].get("scope") == "global")
    rows = {row["name"]: row for row in native("command", {"action": "list"})}
    check("Commands list the base command with scope base", rows["team-ship"].get("scope") == "base" and rows["team-ship"]["readOnly"])
    rows = {row["name"]: row for row in native("skill", {"action": "list"})}
    check("Skills list the base skill with scope base", rows["team-check"].get("scope") == "base" and not rows["team-check"]["managed"])
    rows = {row["name"]: row for row in native("mcp", {"action": "list"})}
    check("MCP lists the base server with scope base, read-only, not plugin-managed",
          rows["teamfs"].get("scope") == "base" and rows["teamfs"]["readOnly"] and not rows["teamfs"]["pluginManaged"])
    hooks = native("hooks", {"action": "list"})["items"]
    base_hook = next(item for item in hooks if item["source"]["path"].startswith(str(team)))
    check("Hooks list the base hook with scope base, not editable",
          base_hook["source"]["scope"] == "base" and base_hook["source"]["editable"] is False)
    refused = cli("desktop", input=json.dumps({"action": "asset", "kind": "hooks", "input": {
        "action": "toggle", "id": base_hook["id"], "revision": base_hook["revision"], "enabled": False}}), ok=False)
    check("Turning off a base hook is refused", refused.returncode != 0 and "base folder are read-only" in refused.stderr)

    # 4. Changes save the person's own version.
    refused = cli("mcp", "remove", "teamfs", ok=False)
    check("Removing a base MCP server is refused with what to do instead",
          refused.returncode != 0 and "comes from the base folder" in refused.stderr and "haarness mcp disable teamfs" in refused.stderr)
    shown = json.loads(cli("mcp", "get", "teamfs").stdout)
    check("haarness mcp get reads the base server, scope base", shown["scope"] == "base")
    cli("mcp", "disable", "teamfs")
    saved = json.loads((config / "haarness.json").read_text())["mcp"]["teamfs"]
    check("Disabling a base server copies its entry, as written, into the global file",
          saved["enabled"] is False and saved["command"][1] == str(mcp) and saved["type"] == "local")
    check("The disabled server is now the global one", json.loads(cli("mcp", "get", "teamfs").stdout)["scope"] == "global")
    until(lambda: any(server["name"] == "teamfs" and server["status"] == "disabled" for server in api("/mcp")),
          "the running server did not reload the disabled MCP server")
    check("The disabled copy is no longer tagged base",
          until(lambda: [server.get("scope") for server in api("/mcp") if server["name"] == "teamfs"] == [None], "scope kept"))
    enabled = cli("mcp", "enable", "teamfs").stdout
    check("Turning it back on removes the copy, so the base folder's applies again",
          "from the base folder" in enabled
          and "teamfs" not in json.loads((config / "haarness.json").read_text()).get("mcp", {})
          and json.loads(cli("mcp", "get", "teamfs").stdout)["scope"] == "base")
    until(lambda: any(server["name"] == "teamfs" and server["status"] == "connected" and server.get("scope") == "base"
                      for server in api("/mcp")), "the base MCP server did not come back")
    write(project / ".haarness/haarness.json", json.dumps({"mcp": {"teamfs": {
        "type": "local", "command": [sys.executable, str(mcp), "PROJECT-" + marker, str(root / "project-mcp-started")]}}}))
    cli("mcp", "disable", "teamfs", "--project")
    project_entry = json.loads((project / ".haarness/haarness.json").read_text())["mcp"]["teamfs"]
    check("A server a project overrides is edited there, and nothing is copied into the global file",
          project_entry["enabled"] is False and "PROJECT-" in project_entry["command"][2]
          and "teamfs" not in json.loads((config / "haarness.json").read_text()).get("mcp", {}))
    (project / ".haarness/haarness.json").unlink()
    native("agent", {"action": "customize", "name": "team-reviewer", "scope": "global",
                     "description": "Reviews like the team, my way", "prompt": "Review my way."})
    check("Customizing a base agent saves a global copy", (config / "agents/team-reviewer.md").is_file())
    rows = {row["name"]: row for row in native("agent", {"action": "list"})}
    check("The customized agent is listed as global", rows["team-reviewer"].get("scope") == "global")

    # 5. The TUI.
    session = "haarness-base-" + uuid.uuid4().hex[:8]
    sessions.append(session)
    launch = e2e_tmux.pane_command(based, [binary, "--workspace", project])
    run("tmux", "new-session", "-d", "-x", "120", "-y", "36", "-s", session, launch)
    pane = lambda: run("tmux", "capture-pane", "-p", "-t", session).stdout
    until(lambda: "/ commands" in pane(), "TUI did not open", 60)

    def slash(command, expected, label):
        run("tmux", "send-keys", "-l", "-t", session, command)
        run("tmux", "send-keys", "-t", session, "Enter")
        shown = until(lambda: expected in pane() and pane(), label)
        (evidence / f"tui-{command.strip('/').split()[0]}.txt").write_text(shown)
        run("tmux", "send-keys", "-t", session, "Escape")
        time.sleep(.4)
        return shown

    shown = slash("/mcp", "teamfs", "/mcp did not list the base server")
    check("The TUI's /mcp lists the base server as base", any("teamfs" in line and "base" in line for line in shown.splitlines()))
    shown = slash("/skills", "team-check", "/skills did not list the base skill")
    check("The TUI's /skills tags the base skill", any("team-check" in line and "base" in line for line in shown.splitlines()))
    shown = slash("/hooks", "UserPromptSubmit", "/hooks did not list the base hook")
    check("The TUI's /hooks lists the base hook as base", any("base" in line for line in shown.splitlines() if "UserPromptSubmit" in line or "base" in line))
    run("tmux", "send-keys", "-l", "-t", session, "/themes")
    run("tmux", "send-keys", "-t", session, "Enter")
    until(lambda: "dracula" in pane().lower() or "theme" in pane().lower(), "the theme picker did not open")
    run("tmux", "send-keys", "-l", "-t", session, "acme")
    shown = until(lambda: "acme-team" in pane() and pane(), "the base theme is not in the theme picker")
    (evidence / "tui-themes.txt").write_text(shown)
    check("The TUI's theme picker offers the base folder's theme", "acme-team" in shown)
    run("tmux", "send-keys", "-t", session, "Escape")
    run("tmux", "kill-session", "-t", session)

    # 6. A client with the variable and a server without it.
    plain_server = json.loads(cli("desktop", input='{"action":"connect"}', environment=env, workspace=other).stdout)
    def server_pid(endpoint):
        request = urllib.request.Request(endpoint["url"] + "/v1/info", headers={"Authorization": "Bearer " + endpoint["token"]})
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.loads(response.read())["pid"]
    first_pid = server_pid(plain_server)
    replaced = json.loads(cli("desktop", input='{"action":"connect"}', workspace=other).stdout)
    second_pid = server_pid(replaced)
    check("A client with the variable replaces an idle server that reads no base folder", second_pid != first_pid)
    slashed = {**env, "HAARNESS_BASE_CONFIG_DIR": str(team) + "/"}
    again = json.loads(cli("desktop", input='{"action":"connect"}', environment=slashed, workspace=other).stdout)
    check("The same base folder written with a trailing / is no change", server_pid(again) == second_pid)
    kept = json.loads(cli("desktop", input='{"action":"connect"}', environment=env, workspace=other).stdout)
    check("A client without the variable never replaces it back", server_pid(kept) == second_pid)
    # A busy server is kept, with one line on what applies the folder.
    def post(endpoint, path, body):
        request = urllib.request.Request(endpoint["url"] + "/v1" + path, data=json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + endpoint["token"], "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.loads(response.read())
    thread = post(kept, "/threads", {"title": "Busy", "provider": "echo", "model": "echo-1"})
    post(kept, f'/threads/{thread["id"]}/turns', {"requestId": str(uuid.uuid4()), "text": "!linger 6000 hello"})
    write(root / "second team/AGENTS.md", "SECOND_TEAM\n")
    second_team = {**env, "HAARNESS_BASE_CONFIG_DIR": str(root / "second team")}
    cli("mcp", "ls", environment=second_team, workspace=other)
    logged = until(lambda: [line for path in state.rglob("*.log*")
                            for line in path.read_text(errors="ignore").splitlines() if "not applied yet" in line],
                   "no notice that the busy server keeps its base folder", 20)
    (evidence / "busy-notice.txt").write_text("\n".join(logged) + "\n")
    check("A busy server that reads another base folder is kept, with one line on when the folder applies",
          server_pid(kept) == second_pid and len(logged) == 1
          and "once that server's work is done" in logged[0] and "serve --stop" not in logged[0])
    relative = {**env, "HAARNESS_BASE_CONFIG_DIR": "team base"}
    refused = cli("doctor", environment=relative, workspace=root)
    (evidence / "relative-notice.txt").write_text(refused.stderr)
    check("A relative base folder is refused with one line and not read",
          refused.stderr.count("must be an absolute path") == 1 and "base config:" not in refused.stdout)

    # Hot reload watches the base folder as it does the global one.
    write(team / "agents/late-arrival.md", "---\ndescription: Added while running\n---\nLate.")
    until(lambda: any(agent["id"] == "late-arrival" for agent in api("/agents")),
          "an agent added to the base folder did not reach the running server", 30)
    (team / "agents/late-arrival.md").unlink()
    until(lambda: all(agent["id"] != "late-arrival" for agent in api("/agents")),
          "an agent removed from the base folder stayed on the running server", 30)
    check("Hot reload picks up an agent added to and removed from the base folder")

    # 9. Never written; a missing folder is reported.
    check("The base folder is byte for byte as it was", snapshot(team) == before)
    missing = {**env, "HAARNESS_BASE_CONFIG_DIR": str(root / "nowhere")}
    check("A missing base folder is reported and nothing breaks",
          "(not found)" in cli("doctor", environment=missing).stdout and "shared" in cli("agents", environment=missing).stdout)
    (evidence / "results.json").write_text(json.dumps(checks, indent=2))
    print(f"{len(checks)} checks passed", flush=True)
finally:
    for session in sessions:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    for workspace, environment in [(project, based), (other, env)]:
        subprocess.run([str(binary), "--workspace", str(workspace), "serve", "--stop"], env=environment,
                       capture_output=True, timeout=60)
    e2e_scratch.settle(root)
