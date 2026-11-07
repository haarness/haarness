#!/usr/bin/env python3
"""Prove generated titles reach a real TUI while its first turn awaits approval.

A second phase sends a 400 KB first message to a mock Anthropic API: the title
request quotes only its first 2,000 characters and caps its output, the turn
itself still sends the whole message, and the TUI shows the title."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid

import e2e_scratch
import e2e_tmux
from e2e_env import clean_env


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs)


def until(check, description):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(description)


binary = str(Path(os.environ.get("HAARNESS_BIN", "target/release/haarness")).resolve())
session = f"haarness-title-{os.getpid()}"
root = Path(tempfile.mkdtemp(prefix="haarness-title-")).resolve()
project = root / "project"
config = project / ".haarness"
config.mkdir(parents=True)
# Chats start in Auto; the first task's ask must wait for a person, so ask for Review.
(config / "config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n[permissions]\nbash="ask"\n'
)
(root / "home").mkdir()
env = {**clean_env(), "HOME": str(root / "home"), "HAARNESS_HOME": str(root / "home"),
       "HAARNESS_DATA_DIR": str(root / "data"), "HAARNESS_TEST_PROVIDER": "1"}
command = e2e_tmux.pane_command(
    {**env, "HAARNESS_DEFAULT_PROVIDER": "echo", "HAARNESS_DEFAULT_MODEL": "echo-1"},
    [binary, "--workspace", str(project), "--trust-project"],
)
try:
    connection = json.loads(run(
        binary, "--workspace", str(project), "--trust-project", "desktop",
        input='{"action":"connect"}', env=env,
    ))

    def api(path):
        request = urllib.request.Request(
            connection["url"].rstrip("/") + "/v1" + path,
            headers={"Authorization": "Bearer " + connection["token"]},
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            return json.load(response)

    run("tmux", "new-session", "-d", "-x", "180", "-y", "45", "-s", session, command)

    def pane():
        return run("tmux", "capture-pane", "-p", "-t", session)

    until(lambda: "h a a r n e s s" in pane(), "TUI did not open")
    run("tmux", "send-keys", "-l", "-t", session, ' !tool bash {"command":"printf title-tmux-proof"}')
    run("tmux", "send-keys", "-t", session, "Enter")
    permission = until(lambda: next(iter(api("/permissions")), None), "First task did not await approval")
    thread = until(
        lambda: next((thread for thread in api("/threads?archived=true")
                      if thread["id"] == permission["threadId"] and thread["title"] not in ("New chat", "New thread")), None),
        "Title was not generated while the task was pending",
    )
    # No assistant response exists yet; Echo can only appear in the newly
    # generated title, which must reach the visible header without a reload.
    def visible_title():
        value = pane()
        return value if "Echo:" in value else None

    captured = until(visible_title, "Generated title did not appear in the TUI")
    assert any(turn["threadId"] == thread["id"] for turn in api("/turns/active"))
    assert not any(item["data"]["type"] == "assistant_message" for item in api(f"/threads/{thread['id']}/items"))
    # The capture stays in the run's folder, which a pass removes unless
    # HAARNESS_E2E_KEEP=1; HAARNESS_TITLE_TMUX_EVIDENCE keeps it elsewhere.
    evidence = Path(os.environ.get("HAARNESS_TITLE_TMUX_EVIDENCE", root / "title-tmux.txt"))
    evidence.write_text(captured)
    print("PASS: generated title is visible in the running TUI before approval"
          + ("" if evidence.parent == root else f"; capture: {evidence}"))
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)

    # A first message of 400 KB, a pasted log: the title is named from its start.
    requests = []

    class Anthropic(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            body = json.dumps({"data": [{"id": "claude-sonnet-4-6", "type": "model"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(body)
            titling = "Write a title for a conversation" in json.dumps(body["messages"])
            text = "Long paste triage" if titling else "LONG_MAIN_ANSWER"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for event in [
                {"type": "message_start", "message": {"usage": {"input_tokens": 9, "output_tokens": 0}}},
                {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
                {"type": "content_block_stop", "index": 0},
                {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}},
                {"type": "message_stop"},
            ]:
                self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
            self.wfile.flush()

    mock = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Anthropic)
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    long_project = root / "long-project"
    (long_project / ".haarness").mkdir(parents=True)
    (long_project / ".haarness" / "config.toml").write_text(
        '[defaults]\nprovider="anthropic"\nmodel="claude-sonnet-4-6"\n[lsp]\nenabled=false\n')
    (root / "long-home").mkdir()
    long_env = {**clean_env(), "HOME": str(root / "long-home"), "HAARNESS_HOME": str(root / "long-home"),
                "HAARNESS_DATA_DIR": str(root / "long-data"), "ANTHROPIC_API_KEY": "fixture-key",
                "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{mock.server_port}/v1"}
    long_args = [binary, "--workspace", str(long_project), "--trust-project"]
    try:
        long_connection = json.loads(run(*long_args, "desktop", input='{"action":"connect"}', env=long_env))

        def long_api(path, body=None):
            request = urllib.request.Request(
                long_connection["url"].rstrip("/") + "/v1" + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={"Authorization": "Bearer " + long_connection["token"], "Content-Type": "application/json"},
            )
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)

        thread = long_api("/threads", {"provider": "anthropic", "model": "claude-sonnet-4-6"})
        run("tmux", "new-session", "-d", "-x", "180", "-y", "45", "-s", session,
            e2e_tmux.pane_command(long_env, [*long_args, "--thread", thread["id"]]))
        until(lambda: "h a a r n e s s" in pane() or "New chat" in pane(), "TUI did not open the chat")
        paste = "Why does the deploy fail? Here is the log:\n" + "deploy step 7: connection refused\n" * 12_000
        assert len(paste) > 400_000
        long_api(f"/threads/{thread['id']}/turns", {"requestId": str(uuid.uuid4()), "text": paste})
        until(lambda: next((t for t in long_api("/threads") if t["id"] == thread["id"]
                            and t["title"] == "Long paste triage"), None), "the long message got no title")
        captured = until(lambda: (lambda value: value if "Long paste triage" in value else None)(pane()),
                         "the title did not reach the TUI")
        until(lambda: any(i["data"].get("text") == "LONG_MAIN_ANSWER"
                          for i in long_api(f"/threads/{thread['id']}/items")), "the long turn got no answer")
        titles = [r for r in requests if "Write a title for a conversation" in json.dumps(r["messages"])]
        assert len(titles) == 1, len(titles)
        content = titles[0]["messages"][0]["content"]
        content = content if isinstance(content, str) else "".join(block.get("text", "") for block in content)
        quoted = content.split("<message>\n", 1)[1].rsplit("\n</message>", 1)[0]
        assert len(quoted) == 2_000 and paste.startswith(quoted), len(quoted)
        assert titles[0]["max_tokens"] == 4_096, titles[0]["max_tokens"]
        assert len(json.dumps(titles[0])) < 20_000, len(json.dumps(titles[0]))
        main = [r for r in requests if r not in titles]
        assert main and paste in json.dumps(main[0]["messages"]).replace("\\n", "\n"), "the turn lost the message"
        long_evidence = Path(os.environ.get("HAARNESS_TITLE_TMUX_EVIDENCE", root / "title-tmux.txt"))
        long_evidence = long_evidence.with_name(long_evidence.stem + "-long" + long_evidence.suffix)
        long_evidence.write_text(captured)
        print("PASS: a 400 KB first message is titled from its first 2,000 characters, output capped, in the TUI")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run([*long_args, "serve", "--stop"], env=long_env, capture_output=True, timeout=30)
        mock.shutdown()
finally:
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    subprocess.run([binary, "--workspace", str(project), "serve", "--stop"],
                   env=env, capture_output=True, timeout=30)
    e2e_scratch.settle(root)
