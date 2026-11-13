#!/usr/bin/env python3
"""Instruction globs end to end: an `instructions` glob that crosses link loops loads at once.

A mock Anthropic Messages server records what a real `haarness run` sends. The person's handbook holds
two links back up its own tree (`team/up -> ..`, `team/here -> .`), a link to a folder kept elsewhere,
a link to a file, a version control folder and a cache folder of many files; the project's docs hold
a loop too. The script proves that:

1. `haarness run` with `"instructions": ["~/handbook/**/RULES.md"]` and a project's
   `"docs/**/*.md"` answers within seconds, and its prompt carries each file those globs match once:
   the linked folder's and the linked file included, nothing from the version control folder;
2. the log has no warning about a glob that stopped early;
3. the terminal UI opens on the same configuration and answers a message (echo provider, tmux);
4. the project's own file points only at files the project holds: its `~/` entry, an absolute path outside it
   and a `../` entry never reach the prompt, each with one line in the log, while its absolute path inside
   the project loads, and an `AGENTS.md` over 40 KiB is left out with a line naming it;
5. the folders' `AGENTS.md` that the scripted `read` calls find attach as before, except one a link leads out
   of the project and one over 40 KiB, which the log names once each.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-instruction-globs.py
"""
import http.server
import json
import os
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import e2e_scratch
from e2e_env import ISOLATED
from e2e_tmux import pane_command

repo = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve()
root = Path(tempfile.mkdtemp(prefix="haarness-instruction-globs-", dir=os.environ.get("HAARNESS_E2E_DIR"))).resolve()
MODEL = "claude-sonnet-4-6"
REQUESTS = []
# The files the scripted turn reads, one in each folder of the project.
READS = []
checks = []
session = f"haarness-instruction-globs-{os.getpid()}"


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
        events = [{"type": "message_start", "message": {"id": f"msg_{len(REQUESTS)}", "usage": {"input_tokens": 9, "output_tokens": 0}}}]
        last = (body.get("messages") or [{}])[-1].get("content")
        answered = isinstance(last, list) and any(block.get("type") == "tool_result" for block in last)
        if body.get("tools") and READS and not answered:
            # The model's turn reads one file in each folder, all at once.
            for index, path in enumerate(READS):
                events += [{"type": "content_block_start", "index": index,
                            "content_block": {"type": "tool_use", "id": f"toolu_read_{index}", "name": "read", "input": {}}},
                           {"type": "content_block_delta", "index": index,
                            "delta": {"type": "input_json_delta", "partial_json": json.dumps({"path": path})}},
                           {"type": "content_block_stop", "index": index}]
            stop = "tool_use"
        else:
            events += [{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                       {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Fixture answer"}},
                       {"type": "content_block_stop", "index": 0}]
            stop = "end_turn"
        events += [{"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 2}},
                   {"type": "message_stop"}]
        for event in events:
            self.wfile.write(("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n").encode())
        self.wfile.flush()


service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Anthropic)
threading.Thread(target=service.serve_forever, daemon=True).start()
home, project, data = root / "home", root / "project", root / "data"
env = {**ISOLATED, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
       "HAARNESS_DATA_DIR": str(data), "USER": os.environ.get("USER", "fixture"), "LANG": "en_US.UTF-8",
       "ANTHROPIC_API_KEY": "fixture-key", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{service.server_port}/v1"}


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


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


def system(body):
    return "".join(block["text"] for block in body["system"]) if isinstance(body["system"], list) else body["system"]


try:
    handbook = home / "handbook"
    marks = {
        "handbook/RULES.md": "MARK_TOP", "handbook/team/RULES.md": "MARK_TEAM",
        "handbook/team/oncall/RULES.md": "MARK_ONCALL", "handbook/.git/RULES.md": "MARK_VCS",
        "elsewhere/legal/RULES.md": "MARK_LINKED_FOLDER", "elsewhere/RULES.md": "MARK_LINKED_FILE",
    }
    for name, mark in marks.items():
        write(home / name, mark + "\n")
    (handbook / ".cache").mkdir()
    for index in range(5000):
        (handbook / f".cache/{index:05}.bin").write_bytes(b"")
    os.symlink("..", handbook / "team/up")
    os.symlink(".", handbook / "team/here")
    os.symlink("../elsewhere/legal", handbook / "legal")
    (handbook / "linked").mkdir()
    os.symlink("../../elsewhere/RULES.md", handbook / "linked/RULES.md")
    write(data / "config/haarness.json", json.dumps({"instructions": ["~/handbook/**/RULES.md"]}))
    project.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(project)], check=True)
    write(project / ".haarness/config.toml", f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    # The project's file also names the person's notes, files beside the project and one of its own by its
    # absolute path: only its own may load.
    write(home / "private-notes.md", "MARK_HOME_PRIVATE\n")
    write(root / "outside/abs.md", "MARK_OUTSIDE_ABSOLUTE\n")
    write(root / "outside/up.md", "MARK_OUTSIDE_UP\n")
    write(project / "guide/inside.md", "MARK_PROJECT_ABSOLUTE\n")
    outside_entries = ["~/private-notes.md", str(root / "outside/abs.md"), "../outside/up.md"]
    write(project / "haarness.json", json.dumps({"instructions": [
        "docs/**/*.md", *outside_entries, str(project / "guide/inside.md")]}))
    write(project / "docs/guide.md", "MARK_PROJECT_DOC\n")
    os.symlink("..", project / "docs/loop")
    # Too large for the prompt: left out whole, and named in the log.
    write(project / "AGENTS.md", "MARK_ROOT_LARGE " * 2700)
    # Folders the scripted turn reads in: a plain AGENTS.md, one that is a link out of the project and
    # one too large.
    write(root / "outside/nested.md", "MARK_NESTED_OUTSIDE\n")
    write(project / "kept/AGENTS.md", "MARK_NESTED_KEPT\n")
    (project / "linked").mkdir()
    os.symlink("../../outside/nested.md", project / "linked/AGENTS.md")
    write(project / "large/AGENTS.md", "MARK_NESTED_LARGE " * 2500)
    for folder in ["kept", "linked", "large"]:
        write(project / folder / "code.rs", f"fn {folder}() {{}} // CODE_{folder.upper()}\n")
        READS.append(f"{folder}/code.rs")

    started = time.monotonic()
    answer = subprocess.run([str(binary), "--workspace", str(project), "--trust-project", "run", "hello",
                             "--provider", "anthropic", "--model", MODEL],
                            text=True, capture_output=True, env=env, timeout=120)
    elapsed = time.monotonic() - started
    assert answer.returncode == 0, answer.stdout + answer.stderr
    turn = next(body for body in REQUESTS if body.get("tools"))
    prompt = system(turn)
    for mark in ["MARK_TOP", "MARK_TEAM", "MARK_ONCALL", "MARK_LINKED_FOLDER", "MARK_LINKED_FILE", "MARK_PROJECT_DOC"]:
        assert prompt.count(mark) == 1, (mark, prompt[-3000:])
    assert "MARK_VCS" not in prompt, prompt[-3000:]
    check(f"`haarness run` answers in {elapsed:.1f} s with each file the globs match once, a linked folder's "
          "and a linked file's included, none from a version control folder")

    logs = "".join(path.read_text(errors="replace") for path in data.rglob("haarness.log*") if path.is_file())
    check("the log has no warning about a glob that stopped early", "stopped after reading" not in logs)

    for mark in ["MARK_HOME_PRIVATE", "MARK_OUTSIDE_ABSOLUTE", "MARK_OUTSIDE_UP"]:
        assert mark not in json.dumps(REQUESTS), mark
    assert prompt.count("MARK_PROJECT_ABSOLUTE") == 1, prompt[-3000:]
    for entry in outside_entries:
        line = f"{project / 'haarness.json'}: ignored instructions {json.dumps(entry)}: it points outside the project"
        assert logs.count(line) == 1, (entry, [row for row in logs.splitlines() if "ignored instructions" in row])
    assert str(data / "config/haarness.json") in logs, "the notice says where the person's own files go"
    check("a project's `~/`, outside absolute and `../` entries reach no request, each with one log line, "
          "while its own absolute path and the person's handbook load")

    assert "MARK_ROOT_LARGE" not in json.dumps(REQUESTS)
    assert f"skipped instructions {project / 'AGENTS.md'}: it is larger than 40 KiB" in logs, logs[-3000:]
    check("an AGENTS.md over 40 KiB is left out of the prompt and named in the log")

    results = next(body for body in REQUESTS if "tool_result" in json.dumps(body.get("messages", [])[-1:]))
    sent = json.dumps(results["messages"])
    for folder in ["KEPT", "LINKED", "LARGE"]:
        assert f"CODE_{folder}" in sent, (folder, sent[:3000])
    assert sent.count("MARK_NESTED_KEPT") == 1, sent[:3000]
    for mark in ["MARK_NESTED_OUTSIDE", "MARK_NESTED_LARGE"]:
        assert mark not in json.dumps(REQUESTS), mark
    left_out = [line for line in logs.splitlines() if "read did not attach an instruction file" in line]
    for name, reason in [("linked", "a link leads it outside the project"), ("large", "it is larger than 40 KiB")]:
        rows = [line for line in left_out if str(project / name / "AGENTS.md") in line]
        assert len(rows) == 1 and reason in rows[0], (name, left_out)
    check("reads attach a folder's AGENTS.md as before, but not one a link leads out of the project or one "
          "over 40 KiB, each named once in the log")

    # The person's glob loads for any project: a plain one, on the echo provider.
    plain = root / "plain"
    plain.mkdir()
    tui_env = {**env, "HAARNESS_TEST_PROVIDER": "1", "HAARNESS_DEFAULT_PROVIDER": "echo", "HAARNESS_DEFAULT_MODEL": "echo-1"}
    command = pane_command(tui_env, [binary, "--workspace", str(plain)])
    subprocess.run(["tmux", "new-session", "-d", "-x", "160", "-y", "40", "-s", session, command], check=True)

    def pane():
        return subprocess.run(["tmux", "capture-pane", "-p", "-t", session], text=True, capture_output=True).stdout

    until(lambda: "h a a r n e s s" in pane(), "the terminal UI did not open", 60)
    (root / "tui-open.txt").write_text(pane())
    subprocess.run(["tmux", "send-keys", "-l", "-t", session, "glob-tmux-proof"], check=True)
    subprocess.run(["tmux", "send-keys", "-t", session, "Enter"], check=True)
    until(lambda: pane().count("glob-tmux-proof") >= 2, "the terminal UI did not answer", 60)
    (root / "tui-answer.txt").write_text(pane())
    check("the terminal UI opens on the looping configuration and answers a message")

    print(f"PASS {len(checks)} instruction glob checks.", flush=True)
finally:
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    for folder in (project, root / "plain"):
        subprocess.run([str(binary), "--workspace", str(folder), "serve", "--stop"], env=env, capture_output=True, timeout=35)
    service.shutdown()
    service.server_close()
    e2e_scratch.settle(root)
