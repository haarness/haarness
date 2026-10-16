#!/usr/bin/env python3
"""Opt-in real-model CLI/TUI chat checks; mutations stay in a disposable project.

Uses OPENAI_API_KEY or the local Haarness API credential in memory. Retains
synthetic transcripts and mock files for inspection, never the credential.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.request

import e2e_scratch


repo = Path(__file__).resolve().parents[1]
binary = str(Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve())
model = os.environ.get("HAARNESS_UX_MODEL", "gpt-5.4-mini")
root = Path(tempfile.mkdtemp(prefix="haarness-chat-ux-"))
project = root / "mock project"
(project / ".haarness").mkdir(parents=True)
(project / "fixtures").mkdir()
(project / "fixtures/ledger.txt").write_text("marker=harbor-528\napples=7\npears=11\n", encoding="utf-8")
(project / "fixtures/calc.py").write_text("def average(values):\n    return sum(values) / len(values)\n", encoding="utf-8")
(project / "AGENTS.md").write_text("This is a disposable chat test. Only access this project. Keep fixtures unchanged. Put any requested generated files in output/. Do not access credentials, network resources or other directories.\n")
(project / ".haarness/config.toml").write_text(f'[defaults]\nprovider="openai"\nmodel="{model}"\n[permissions]\nread="allow"\nwrite="allow"\nedit="allow"\nbash="allow"\n')
subprocess.run(["git", "init", "-q", str(project)], check=True)
env = {k: v for k, v in os.environ.items() if not k.startswith("HAARNESS_")}
env["HAARNESS_DATA_DIR"] = str(root / "state")
if not env.get("OPENAI_API_KEY"):
    credentials = Path.home() / ".local/share/haarness/auth.json"
    auth = json.loads(credentials.read_text()).get("openai", {}) if credentials.exists() else {}
    if auth.get("type") == "api":
        env["OPENAI_API_KEY"] = auth["key"]
if not env.get("OPENAI_API_KEY"):
    raise SystemExit("Set OPENAI_API_KEY or sign in with a Haarness OpenAI API credential")
args = [binary, "--workspace", str(project), "--trust-project"]
session = f"haarness-chat-ux-{os.getpid()}"
checks = []
connection = None


def run(command, **kwargs):
    return subprocess.run(command, text=True, capture_output=True, check=True, **kwargs).stdout


def api(path, body=None):
    req = urllib.request.Request(connection["url"].rstrip("/") + "/v1" + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=15) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def until(check, label, timeout=150):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.15)
    raise AssertionError(label)


def record(label, detail=None):
    checks.append({"name": label, "detail": detail})
    (root / "results.json").write_text(json.dumps({"model": model, "checks": checks}, indent=2))
    print("PASS", label, flush=True)


def items(thread):
    result = api(f"/threads/{thread}/items")
    (root / f"{thread}.json").write_text(json.dumps(result, indent=2))
    return result


def calls(thread):
    return [item["data"] for item in items(thread) if item["data"]["type"] == "tool_call"]


def chat(label, prompt, *options):
    before = {t["id"] for t in api("/threads?archived=true")}
    started = time.monotonic()
    result = subprocess.run(args + ["run", "--provider", "openai", "--model", model, "--no-thinking", *options],
                            input=prompt, env=env, text=True, capture_output=True, timeout=180)
    (root / f"{label}.stdout").write_text(result.stdout)
    (root / f"{label}.stderr").write_text(result.stderr)
    assert result.returncode == 0, f"{label}: exit {result.returncode}; see {root}"
    assert result.stdout.strip(), f"{label}: blank output"
    new = [t["id"] for t in api("/threads?archived=true") if t["id"] not in before and ("--fork" in options or not t.get("parentId"))]
    thread = options[options.index("--session") + 1] if "--session" in options and "--fork" not in options else new[0]
    saved = items(thread)
    assert not any(i["data"]["type"] == "error" for i in saved), f"{label}: persisted error"
    record(label, {"thread": thread, "seconds": round(time.monotonic() - started, 2)})
    return thread, result.stdout


def fixture_hashes():
    return {str(p.relative_to(project)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (project / "fixtures").rglob("*") if p.is_file()}


def pane():
    return run(["tmux", "capture-pane", "-p", "-t", session])


def tui_chat(thread, prompt, label):
    before = len(items(thread))
    run(["tmux", "load-buffer", "-"], input=prompt)
    run(["tmux", "paste-buffer", "-p", "-t", session])
    run(["tmux", "send-keys", "-t", session, "Enter"])
    until(lambda: len(api(f"/threads/{thread}/items")) > before, label + " was not submitted")
    until(lambda: not api("/turns/active"), label + " stayed active")
    saved = items(thread)[before:]
    assert any(i["data"]["type"] == "assistant_message" for i in saved), label + " had no response"
    assert not any(i["data"]["type"] == "error" for i in saved), label + " persisted error"
    capture = until(lambda: pane() if "Ready" in pane() else None, label + " never became ready")
    (root / f"{label}.txt").write_text(capture)
    record(label, {"thread": thread})
    return saved


print("Evidence:", root, flush=True)
try:
    connection = json.loads(run(args + ["desktop"], input='{"action":"connect"}', env=env, timeout=60))
    original = fixture_hashes()
    thread, out = chat("cli-read", "Read fixtures/ledger.txt with a tool. Report the marker and the sum of apples and pears. Do not change any files.", "--agent", "explore")
    assert "harbor-528" in out and "18" in out and calls(thread), "read result lacks evidence"
    _, out = chat("cli-continue", "Without reading again, what was the marker and total? Reply briefly.", "--session", thread)
    assert "harbor-528" in out and "18" in out
    before_fork = len(items(thread))
    fork, out = chat("cli-fork-json", "Recall the marker from our conversation. Reply with only the marker.", "--session", thread, "--fork", "--format", "json")
    assert fork != thread and len(items(thread)) == before_fork
    records = [json.loads(line) for line in out.splitlines() if line.strip()]
    assert records and "harbor-528" in out
    record("fork leaves source unchanged and stdout is valid JSONL")
    review, out = chat("cli-review", "Read fixtures/calc.py and explain the empty-list bug with a file and line reference. This is read-only; do not fix it.", "--agent", "review")
    assert "calc.py" in out and ("zero" in out.lower() or "empty" in out.lower()) and calls(review)
    assert fixture_hashes() == original
    assert not (project / "output").exists(), "read-only chats created output"
    record("read-only files unchanged")
    delegated, out = chat("cli-delegation", "Use the task tool to ask two independent explore agents to read files: one reads fixtures/ledger.txt and reports its marker; the other reads fixtures/calc.py and reports the empty-list behavior. Create separate children by omitting task_id. Keep all work read-only. Wait for both and summarize their findings.", "--wait-background")
    children = [t for t in api("/threads?archived=true") if t.get("parentId") == delegated]
    assert len(children) == 2, "independent assignments did not produce two children"
    for child in children:
        assert calls(child["id"]), "child has no persisted tool use"
    assert not any(i["data"]["type"] == "tool_result" and i["data"].get("is_error") for i in items(delegated))
    assert fixture_hashes() == original
    record("two real delegated readers completed without conflicts")
    if "--extras-only" in sys.argv:
        print(f"PASS: {len(checks)} real-model checks; evidence {root}", flush=True)
        raise SystemExit(0)
    writer, out = chat("cli-write-and-run", 'Create output/space dir/receipt 日本語.txt containing exactly "harbor-528\\napples+pears=18\\n" (actual newlines). Use a write tool, then read it back and run a Python assertion checking its exact UTF-8 content. Work only in output/. Report the verification result.')
    expected = "harbor-528\napples+pears=18\n"
    assert (project / "output/space dir/receipt 日本語.txt").read_text() == expected
    assert any(c["name"].lower() in ["bash", "functions.bash"] for c in calls(writer)), "model did not run verification"
    _, out = chat("cli-edit", 'Edit output/space dir/receipt 日本語.txt: change apples+pears=18 to total=18 and append verified=yes on its own line. Preserve the marker. Read it back and verify.', "--session", writer)
    assert (project / "output/space dir/receipt 日本語.txt").read_text() == "harbor-528\ntotal=18\nverified=yes\n"
    t = api("/threads", {"title":"Live TUI UX", "provider":"openai", "model":model})["id"]
    # Credentials are inherited from this process by the fixture daemon; tmux
    # only needs its isolated state path, never a key in its command line.
    command = shlex.join(["env", f"HAARNESS_DATA_DIR={env['HAARNESS_DATA_DIR']}", *args, "--thread", t])
    run(["tmux", "new-session", "-d", "-x", "160", "-y", "48", "-s", session, command])
    until(lambda: "h a a r n e s s" in pane(), "TUI did not open")
    data = tui_chat(t, "Read fixtures/ledger.txt. Reply with its marker, then a Markdown table of the two fruit counts. Do not change files.", "tui-read-markdown")
    text = "\n".join(i["data"].get("text", "") for i in data if i["data"]["type"] == "assistant_message")
    assert "harbor-528" in text and "|" in text and calls(t)
    data = tui_chat(t, "Write output/tui-note.txt containing exactly tui-write-proof on one line with a trailing newline, then read the file and verify. Do not alter fixtures.", "tui-write-readback")
    assert (project / "output/tui-note.txt").read_text() == "tui-write-proof\n"
    assert fixture_hashes() == original
    record("fixtures preserved across all writes")
    run(["tmux", "kill-session", "-t", session])
    run(["tmux", "new-session", "-d", "-x", "160", "-y", "48", "-s", session, command])
    until(lambda: "tui-write-proof" in pane(), "reopened TUI lost completed response")
    (root / "tui-reopened.txt").write_text(pane())
    record("TUI reopen preserves chat")
    print(f"PASS: {len(checks)} real-model checks; evidence {root}", flush=True)
finally:
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    if connection:
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=30)
    e2e_scratch.settle(root)
