#!/usr/bin/env python3
"""Real tmux proof: a provider slow to list its models never holds the TUI.

Each phase starts a fresh workspace server from the TUI, with providers
whose model lists are slow to come:

- stalled: Anthropic (`ANTHROPIC_BASE_URL`) and a configured OpenAI-compatible
  provider point at a port that accepts connections and never answers. The
  server used to fetch the built-in providers' lists, then the configured
  ones', before it listened, each round costing its whole timeout (with
  Haarness Cloud signed in, a third), so the TUI drew nothing for 6 s or more,
  or its launcher gave up after 8 s. Now one round of at most 3 s runs while
  the server starts, and a turn runs on the echo provider.
- late: the only provider is a configured Anthropic-format one with no
  built-in list, which lists its models in three pages of 2.8 s each. The
  TUI draws first and says it is loading models (not that nothing is
  connected); when the list comes, the new chat takes that provider's model
  (not the server's default, which nobody connected) and is ready, without a
  restart or a picker opened.
- run: `haarness run --provider` with that provider, on a server that has
  just started: the run waits for the list and answers from the provider's
  first model, instead of saying the provider lists no models.
"""

import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import e2e_scratch

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()


def tmux(*args):
    return subprocess.check_output(["tmux", *args], text=True).strip()


def wait(predicate, label, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return time.monotonic()
        time.sleep(0.1)
    raise AssertionError("Timed out: " + label)


class LateModels(BaseHTTPRequestHandler):
    """`GET /v1/models` in Anthropic's shape: three pages, each taking 2.8 s.
    `POST /v1/messages` answers "late hello"."""

    def log_message(self, *_):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        events = [
            ("message_start", {"type": "message_start", "message": {
                "id": "msg_fixture", "type": "message", "role": "assistant", "model": "late-model-1",
                "content": [], "stop_reason": None, "usage": {"input_tokens": 1, "output_tokens": 0}}}),
            ("content_block_start", {"type": "content_block_start", "index": 0,
                                     "content_block": {"type": "text", "text": ""}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0,
                                     "delta": {"type": "text_delta", "text": "late hello"}}),
            ("content_block_stop", {"type": "content_block_stop", "index": 0}),
            ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                               "usage": {"output_tokens": 2}}),
            ("message_stop", {"type": "message_stop"}),
        ]
        body = "".join(f"event: {name}\ndata: {json.dumps(data)}\n\n" for name, data in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        time.sleep(2.8)
        page = 3 if "after_id=page-2" in self.path else 2 if "after_id=page-1" in self.path else 1
        page = {"data": [{"id": f"late-model-{page}", "display_name": f"Late model {page}", "type": "model"}],
                "has_more": page < 3, "last_id": f"page-{page}"}
        body = json.dumps(page).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


root = Path(tempfile.mkdtemp(prefix="haarness-first-frame-")).resolve()
sessions = []
servers = []


def phase(name, protocol, base_url, echo, extra=()):
    workspace = root / name / "workspace"
    (workspace / ".haarness").mkdir(parents=True)
    defaults = '[defaults]\nprovider="echo"\nmodel="echo-1"\n' if echo else ""
    (workspace / ".haarness/config.toml").write_text(
        defaults
        + f'[providers.slowlist]\nprotocol="{protocol}"\nname="Slow list"\n'
        + f'base_url="{base_url}"\napi_key_env="SLOWLIST_KEY"\n[lsp]\nenabled=false\n'
    )
    data, home = root / name / "data", root / name / "home"
    home.mkdir()
    session = f"haarness-first-frame-{name}-{os.getpid()}"
    target = session + ":0.0"
    variables = [
        "HAARNESS_DATA_DIR=" + str(data),
        "HOME=" + str(home),
        "HAARNESS_HOME=" + str(home),
        "SLOWLIST_KEY=fixture-key",
        "HAARNESS_NO_CONNECT_PROMPT=1",
        *extra,
    ] + (["HAARNESS_TEST_PROVIDER=1"] if echo else [])
    servers.append((workspace, {**os.environ, **dict(item.split("=", 1) for item in variables)}))
    command = shlex.join(["env", *variables, str(BINARY), "--workspace", str(workspace), "--trust-project"])
    tmux("new-session", "-d", "-s", session, "-x", "110", "-y", "32")
    sessions.append(session)
    tmux("send-keys", "-t", target, "-l", command)
    started = time.monotonic()
    tmux("send-keys", "-t", target, "Enter")
    drawn = wait(
        lambda: tmux("display-message", "-p", "-t", target, "#{alternate_on}") == "1"
        and "Ask anything" in tmux("capture-pane", "-p", "-t", target),
        f"{name}: the first frame",
    )
    return target, started, drawn - started


try:
    # Accepts connections into its backlog and never answers them.
    stalled = socket.socket()
    stalled.bind(("127.0.0.1", 0))
    stalled.listen(64)
    port = stalled.getsockname()[1]
    target, _, took = phase("stalled", "openai-chat-completions", f"http://127.0.0.1:{port}/v1", echo=True,
                            extra=["ANTHROPIC_API_KEY=fixture-key", f"ANTHROPIC_BASE_URL=http://127.0.0.1:{port}"])
    assert took < 7, f"the first frame took {took:.1f} s"
    tmux("send-keys", "-t", target, "-l", "hello first frame")
    tmux("send-keys", "-t", target, "Enter")
    wait(lambda: "Echo: hello first frame" in tmux("capture-pane", "-p", "-t", target), "stalled: the echo reply")
    print(f"PASS the TUI draws in {took:.1f} s and runs a turn while a provider never lists its models", flush=True)

    late = ThreadingHTTPServer(("127.0.0.1", 0), LateModels)
    threading.Thread(target=late.serve_forever, daemon=True).start()
    target, started, took = phase("late", "anthropic-messages", f"http://127.0.0.1:{late.server_port}/v1",
                                  echo=False)
    assert took < 7, f"the first frame took {took:.1f} s"
    pane = tmux("capture-pane", "-p", "-t", target)
    assert "Loading models…" in pane and "Connect a provider" not in pane, pane
    filled = wait(
        lambda: "slowlist/" in tmux("capture-pane", "-p", "-t", target)
        and "Ready" in tmux("capture-pane", "-p", "-t", target),
        "late: the new chat takes the late list's model",
    )
    pane = tmux("capture-pane", "-p", "-t", target)
    assert "is not connected" not in pane and "Connect a provider" not in pane, pane
    # The list is in about 8.5 s after launch.
    assert 7.5 <= filled - started < 12, f"the list showed {filled - started:.1f} s after launch"
    print(f"PASS the TUI draws in {took:.1f} s and fills in the late list {filled - started:.1f} s after launch", flush=True)

    # A run on a server that has only just started waits for the list.
    workspace = root / "run" / "workspace"
    (workspace / ".haarness").mkdir(parents=True)
    (workspace / ".haarness/config.toml").write_text(
        '[providers.slowlist]\nprotocol="anthropic-messages"\nname="Slow list"\n'
        + f'base_url="http://127.0.0.1:{late.server_port}/v1"\napi_key_env="SLOWLIST_KEY"\n[lsp]\nenabled=false\n')
    (root / "run" / "home").mkdir()
    env = {**os.environ, "HAARNESS_DATA_DIR": str(root / "run" / "data"), "HOME": str(root / "run" / "home"),
           "HAARNESS_HOME": str(root / "run" / "home"), "SLOWLIST_KEY": "fixture-key"}
    env.pop("HAARNESS_TEST_PROVIDER", None)
    servers.append((workspace, env))
    ran = subprocess.run([str(BINARY), "--workspace", str(workspace), "--trust-project", "run", "--provider",
                          "slowlist", "hello"], env=env, capture_output=True, text=True, timeout=120)
    assert ran.returncode == 0 and "late hello" in ran.stdout, ran.stdout + ran.stderr
    print("PASS haarness run waits for a provider's late list and answers from its first model", flush=True)
finally:
    for session in sessions:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    for workspace, env in servers:
        subprocess.run([str(BINARY), "--workspace", str(workspace), "serve", "--stop"], env=env,
                       capture_output=True, timeout=60)
    e2e_scratch.settle(root)
