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
- late: a configured Anthropic-format provider lists its models in three
  pages of 2.8 s each, past the 3 s the server waits. The TUI draws first and says to connect a
  provider; when the list comes the server announces it and the
  TUI shows the provider connected, without a restart or a picker opened.
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
    """`GET /v1/models` in Anthropic's shape: three pages, each taking 2.8 s."""

    def log_message(self, *_):
        pass

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
    assert took < 6, f"the first frame took {took:.1f} s"
    tmux("send-keys", "-t", target, "-l", "hello first frame")
    tmux("send-keys", "-t", target, "Enter")
    wait(lambda: "Echo: hello first frame" in tmux("capture-pane", "-p", "-t", target), "stalled: the echo reply")
    print(f"PASS the TUI draws in {took:.1f} s and runs a turn while a provider never lists its models", flush=True)

    late = ThreadingHTTPServer(("127.0.0.1", 0), LateModels)
    threading.Thread(target=late.serve_forever, daemon=True).start()
    target, started, took = phase("late", "anthropic-messages", f"http://127.0.0.1:{late.server_port}/v1",
                                  echo=False)
    assert took < 6, f"the first frame took {took:.1f} s"
    assert "Connect a provider to start" in tmux("capture-pane", "-p", "-t", target), tmux("capture-pane", "-p", "-t", target)
    filled = wait(
        lambda: "is not connected · /connect" in tmux("capture-pane", "-p", "-t", target),
        "late: the provider shows connected once its list comes",
    )
    # The list is in about 8.5 s after launch.
    assert 7.5 <= filled - started < 11, f"the list showed {filled - started:.1f} s after launch"
    print(f"PASS the TUI draws in {took:.1f} s and fills in the late list {filled - started:.1f} s after launch", flush=True)
finally:
    for session in sessions:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    for workspace, env in servers:
        subprocess.run([str(BINARY), "--workspace", str(workspace), "serve", "--stop"], env=env,
                       capture_output=True, timeout=60)
    e2e_scratch.settle(root)
