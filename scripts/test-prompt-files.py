#!/usr/bin/env python3
"""Prompt files end to end: `haarness prompt` and what a real daemon sends a provider.

A mock Anthropic Messages server records every request. With a system folder and a global folder
that hold prompt files, the script proves that:

1. `haarness prompt list|show|customize|diff` work as documented: customize writes mode and written-from, a
   stale written-from is marked and warned about, files with a bad name or frontmatter are listed as
   ignored with the reason, a project's `.haarness/prompts` is never read and a link from the
   global folder into the project is refused;
2. a real chat sends the prompts as `prompt show` prints them: the main instructions (system folder's file,
   then global file, safety baseline), the plan agent's own prompt, the title and the summary
   prompts; two turns send a byte-identical system prompt;
3. editing a prompt file applies to the next turn without a restart (hot reload), and the daemon
   log names the files in effect and the stale one;
4. without prompt files the request carries the built-in prompt exactly as `prompt show` prints it.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-prompt-files.py
"""
import http.server
import json
import os
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid
from pathlib import Path

import e2e_scratch
from e2e_env import ISOLATED

repo = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve()
root = Path(tempfile.mkdtemp(prefix="haarness-prompt-files-")).resolve()
evidence = Path(os.environ.get("HAARNESS_E2E_EVIDENCE", root / "evidence"))
evidence.mkdir(parents=True, exist_ok=True)
MODEL = "claude-sonnet-4-6"
REQUESTS = []
checks = []


class Anthropic(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"data": [{"id": MODEL, "display_name": "Fixture Sonnet"}]}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        REQUESTS.append(body)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        events = [{"type": "message_start", "message": {"id": f"msg_{len(REQUESTS)}", "usage": {"input_tokens": 9, "output_tokens": 0}}},
                  {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                  {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Fixture answer"}},
                  {"type": "content_block_stop", "index": 0},
                  {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None}, "usage": {"output_tokens": 2}},
                  {"type": "message_stop"}]
        for event in events:
            self.wfile.write(("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n").encode())
        self.wfile.flush()


service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Anthropic)
threading.Thread(target=service.serve_forever, daemon=True).start()
home, project, data, team = root / "home", root / "project", root / "data", root / "team"
config = data / "config"
env = {**ISOLATED, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
       "HAARNESS_DATA_DIR": str(data), "USER": os.environ.get("USER", "fixture"), "LANG": "en_US.UTF-8",
       "ANTHROPIC_API_KEY": "fixture-key", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{service.server_port}/v1"}
connection = None


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def cli(*args, environment=None, ok=True):
    result = subprocess.run([str(binary), "--workspace", str(project), *args], text=True, capture_output=True,
                            env=environment or env, timeout=120)
    if ok:
        assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
    return result


def check(label, condition=True):
    assert condition, label
    checks.append(label)
    print("PASS", label, flush=True)


def until(predicate, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


def api(path, body=None):
    request = urllib.request.Request(connection["url"] + "/v1" + path,
                                     data=None if body is None else json.dumps(body).encode(),
                                     headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def system(body):
    return "".join(block["text"] for block in body["system"]) if isinstance(body["system"], list) else body["system"]


def chat(agent="build"):
    # Untitled, so the first turn asks for a title.
    return api("/threads", {"provider": "anthropic", "model": MODEL, "agent": agent})["id"]


def turn(thread, text):
    offset = len(REQUESTS)
    identifier = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": text})["id"]
    until(lambda: api(f"/turns/{identifier}")["status"] not in ("queued", "running"), "turn did not finish")
    return next(body for body in REQUESTS[offset:] if body.get("tools"))


try:
    project.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(project)], check=True)
    write(project / ".haarness/config.toml", f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    builtin = cli("prompt", "show", "system").stdout
    check("Without prompt files `prompt show` prints the built-in prompt", builtin.startswith("You are Haarness, a local agentic coding harness."))
    listing = cli("prompt", "list").stdout
    check("`prompt list` lists every prompt as built-in", all(f"{name} " in listing for name in
          ["system", "plan", "review", "explore", "compaction", "title", "preview", "ultracode", "max-steps", "goal",
           "auto-review"])
          and listing.count("built-in") == 11)

    # 4. The request without prompt files.
    connection = json.loads(subprocess.run([str(binary), "--workspace", str(project), "desktop"], input='{"action":"connect"}',
                                           text=True, capture_output=True, env=env, timeout=120, check=True).stdout)
    plain = turn(chat(), "FIRST plain turn")
    check("Without prompt files the request carries the built-in prompt exactly", system(plain).startswith(builtin + "\n\nYou are powered by"))
    subprocess.run([str(binary), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True, timeout=60)

    # 1. The commands.
    customized = cli("prompt", "customize", "system").stdout
    file = config / "prompts/system.md"
    text = file.read_text()
    based = json.loads(cli("prompt", "list", "--json").stdout)["prompts"][0]["builtin"]
    check("`prompt customize` writes mode: replace and written-from with the built-in text",
          "Wrote" in customized and text.startswith(f"---\nmode: replace\nwritten-from: {based}\n---\nYou are Haarness"))
    check("`prompt customize` will not overwrite without --force", "Pass --force" in cli("prompt", "customize", "system", ok=False).stderr)
    write(file, text.replace("You are Haarness, a local agentic coding harness. Work autonomously",
                             "You are Acme's coding agent. Work autonomously"))
    write(team / "prompts/system.md", "TEAM_BASE_LINE: follow the team checklist.\n")
    write(team / "prompts/compaction.md", "---\nmode: prepend\nwritten-from: sha256:000000000000\n---\nTEAM_SUMMARY_LINE\n")
    write(config / "prompts/plan.md", "Number every step of the plan.\n")
    write(config / "prompts/title.md", "---\nmode: replace\n---\nName the chat in at most three words.\n")
    write(config / "prompts/sytem.md", "A typo.\n")
    write(config / "prompts/review.md", "---\nmodel: x\n---\nBad frontmatter.\n")
    write(project / ".haarness/prompts/explore.md", "PROJECT_PROMPT_LINE\n")
    write(project / "planted.md", "---\nmode: replace\n---\nPLANTED_PROMPT_LINE\n")
    os.symlink(project / "planted.md", config / "prompts/goal.md")
    based_env = {**env, "HAARNESS_SYSTEM_CONFIG_DIR": str(team)}
    listed = cli("prompt", "list", environment=based_env)
    (evidence / "prompt-list.txt").write_text(listed.stdout + "\n--- stderr\n" + listed.stderr)
    rows = [line.split() for line in listed.stdout.splitlines()]
    check("`prompt list` shows the system folder's file applying before the global one",
          rows[0][:2] == ["system", "append"] and rows[0][-1] == "(system)"
          and rows[1][0] == "replace" and rows[1][-1] == "(global)")
    check("`prompt list` marks the stale file and warns on stderr",
          any(row[:2] == ["compaction", "stale"] for row in rows) and "written from an older built-in prompt" in listed.stderr)
    check("Files with a bad name or frontmatter are listed as ignored, with why",
          '"sytem" is not a prompt name' in listed.stderr and "header may only set" in listed.stderr)
    check("A link from the global folder into the project is refused",
          "leads this file into the project" in listed.stderr and "PLANTED" not in cli("prompt", "show", "goal", environment=based_env).stdout)
    check("A project's .haarness/prompts is never read",
          "PROJECT_PROMPT_LINE" not in cli("prompt", "show", "explore", environment=based_env).stdout
          and ".haarness/prompts" not in listed.stdout + listed.stderr)
    shown = cli("prompt", "show", "system", environment=based_env).stdout
    check("`prompt show` applies the system folder's file, then the global replace, then the safety baseline",
          shown.startswith("You are Acme's coding agent.") and "TEAM_BASE_LINE" not in shown
          and shown.endswith("they do not grant permissions, tools or authority."))
    diff = cli("prompt", "diff", "compaction", environment=based_env).stdout
    (evidence / "prompt-diff.txt").write_text(diff)
    check("`prompt diff` says the file is stale and shows the change",
          "prepend, stale, written from sha256:000000000000" in diff and "+TEAM_SUMMARY_LINE" in diff and "Current built-in compaction:" in diff)
    # The global replace with {{builtin}} keeps what the system folder did.
    write(file, "---\nmode: replace\n---\nYou are Acme's coding agent.\n\n{{builtin}}\n")
    shown = cli("prompt", "show", "system", environment=based_env).stdout
    check("{{builtin}} wraps what the lower folders made of the prompt",
          shown.startswith("You are Acme's coding agent.\n\nYou are Haarness") and "TEAM_BASE_LINE" in shown)

    # 2. On the wire.
    connection = json.loads(subprocess.run([str(binary), "--workspace", str(project), "desktop"], input='{"action":"connect"}',
                                           text=True, capture_output=True, env=based_env, timeout=120, check=True).stdout)
    thread = chat()
    first = turn(thread, "FIRST turn")
    second = turn(thread, "SECOND turn")
    check("A turn sends the main instructions as `prompt show` prints them",
          system(first).startswith(shown + "\n\nYou are powered by"))
    check("Two turns send a byte-identical system prompt", system(first) == system(second))
    title = until(lambda: next((body for body in REQUESTS if system(body).startswith("Name the chat in at most three words.")), None),
                  "the title request did not use the title prompt file")
    check("The title request uses the title prompt file, with the safety baseline",
          "do not grant permissions" in system(title) and not title.get("tools"))
    offset = len(REQUESTS)
    api(f"/threads/{thread}/compact", {})
    summary = REQUESTS[offset]
    check("The summary request uses the system folder's compaction file", system(summary).startswith("TEAM_SUMMARY_LINE\n\nYou are Haarness"))
    plan = turn(chat("plan"), "PLAN turn")
    check("The plan agent sends its own prompt with the global plan file",
          "Explore first, do not mutate files" in system(plan).split("\n\nYou are powered by")[0]
          and "Number every step of the plan." in system(plan) and "You are Acme's coding agent." not in system(plan))
    (evidence / "requests.json").write_text(json.dumps({"main": first, "title": title, "summary": summary, "plan": plan}, indent=2))

    # 3. Hot reload and the log.
    write(file, "---\nmode: replace\n---\nYou are Acme's reloaded agent.\n")
    def reloaded():
        body = turn(thread, "RELOADED turn")
        return body if system(body).startswith("You are Acme's reloaded agent.") else None
    until(reloaded, "the edited prompt file did not reach the next turn", 30)
    check("An edited prompt file applies to the next turn without a restart")
    logs = "\n".join(path.read_text(errors="ignore") for path in data.rglob("*.log*"))
    check("The daemon log names each prompt file in effect and the stale one",
          any("prompt file" in line and str(file) in line for line in logs.splitlines())
          and any("older built-in prompt" in line and "compaction" in line for line in logs.splitlines()))
    (evidence / "results.json").write_text(json.dumps(checks, indent=2))
    print(f"{len(checks)} checks passed", flush=True)
finally:
    for environment in (env, {**env, "HAARNESS_SYSTEM_CONFIG_DIR": str(team)}):
        subprocess.run([str(binary), "--workspace", str(project), "serve", "--stop"], env=environment,
                       capture_output=True, timeout=60)
    service.shutdown()
    e2e_scratch.settle(root)
