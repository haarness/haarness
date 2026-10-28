#!/usr/bin/env python3
"""A configured reasoning effort and the model's own default, through a real TUI in tmux.

The project sets `defaults.reasoning_effort = "high"` and talks to a local OpenAI-compatible
fixture that lists three levels and records every request it is sent. The TUI must show high as
the level in use (marked as the default), send it, then send no effort at all once provider
default is chosen, and show that choice in `/reasoning`.
"""
import http.server
import json
import os
import pathlib
import subprocess
import tempfile
import threading
import time

import e2e_scratch
import e2e_tmux

binary = pathlib.Path(os.environ.get("HAARNESS_BIN", "target/debug/haarness")).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix="haarness-reasoning-default-")).resolve()
workspace = root / "workspace"
(workspace / ".haarness").mkdir(parents=True)
captures = pathlib.Path(os.environ.get("HAARNESS_E2E_CAPTURES", root / "captures"))
captures.mkdir(parents=True, exist_ok=True)
received = []
lock = threading.Lock()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps(
            {
                "data": [
                    {
                        "id": "effort-model",
                        "supported_parameters": ["tools", "reasoning_effort"],
                        "reasoning_efforts": ["low", "medium", "high"],
                    }
                ]
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        with lock:
            received.append(request)
        chunks = [
            {"choices": [{"index": 0, "delta": {"content": "effort-reply"}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        body = "".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks) + "data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(body.encode())


service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=service.serve_forever, daemon=True).start()
(workspace / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="effort-fixture"\nmodel="effort-model"\nreasoning_effort="high"\n'
    'approval_mode="review"\n'
    '[providers.effort-fixture]\nname="Effort fixture"\nprotocol="openai-chat-completions"\n'
    f'base_url="http://127.0.0.1:{service.server_port}/v1"\nrequires_auth=false\ntools=true\n'
    'reasoning=true\n[providers.ollama]\nenabled=false\n[providers.lmstudio]\nenabled=false\n'
)
name = f"haarness-reasoning-default-{os.getpid()}"
env = {
    **os.environ,
    "HAARNESS_DATA_DIR": str(root / "data"),
    "HAARNESS_HOME": str(root / "home"),
}
for key in ["HAARNESS_TEST_PROVIDER", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"]:
    env.pop(key, None)


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout


def capture():
    return run(["tmux", "capture-pane", "-p", "-t", name + ":0.0"])


def keys(*values):
    run(["tmux", "send-keys", "-t", name + ":0.0", *values])


def command(value):
    keys("C-u")
    run(["tmux", "send-keys", "-l", "-t", name + ":0.0", value])
    keys("Enter")


def until(predicate, label):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise AssertionError(label + "\n" + capture())


def turn_request(marker):
    """The provider request of the turn that carries the prompt `marker`; a chat's title is
    asked for with the prompt too, in a request of its own."""
    with lock:
        for request in received:
            if "Write a title" in json.dumps(request.get("messages")):
                continue
            if any(
                message.get("role") == "user" and marker in json.dumps(message.get("content"))
                for message in request.get("messages", [])
            ):
                return request
    return None


replies = 0


def send(marker):
    global replies
    replies += 1
    command(marker)
    until(lambda: turn_request(marker) is not None, f"{marker} never reached the provider")
    until(lambda: capture().count("effort-reply") >= replies, f"{marker} got no reply")
    return turn_request(marker)


def picker(label):
    command("/reasoning")
    until(lambda: "Let the model decide" in capture(), "Reasoning picker did not open")
    screen = capture()
    (captures / f"{label}.txt").write_text(screen)
    keys("Escape")
    until(lambda: "Let the model decide" not in capture(), "Reasoning picker did not close")
    return screen


def row(screen, text):
    return next((line for line in screen.splitlines() if text in line), "")


endpoint = None
try:
    endpoint = json.loads(
        run(
            [str(binary), "--workspace", str(workspace), "--trust-project", "desktop"],
            env=env,
            input='{"action":"connect"}',
            timeout=90,
        )
    )
    args = [str(binary), "--workspace", str(workspace), "--trust-project"]
    run(["tmux", "new-session", "-d", "-x", "120", "-y", "36", "-s", name, e2e_tmux.pane_command(env, args)])
    until(lambda: "effort-model" in capture() or "Effort" in capture(), "TUI did not start on the fixture model")

    # No pick: high is the level in use, marked as the configured default.
    screen = picker("configured")
    high = row(screen, "Deep thinking")
    assert "default" in high and "●" in high, high
    assert "●" not in row(screen, "Let the model decide"), screen
    first = send("first-configured")
    assert first.get("reasoning_effort") == "high", first

    # Provider default overrides it, with no badge; the request names no effort.
    command("/reasoning off")
    until(lambda: "Reasoning effort: provider default" in capture(), "Provider default was not chosen")
    second = send("second-model-default")
    assert "reasoning_effort" not in second, second
    screen = picker("model-default")
    assert "●" in row(screen, "Let the model decide"), screen
    high = row(screen, "Deep thinking")
    assert "default" in high and "●" not in high, high

    # The mark is translated with the rest of the row, right to left in Arabic.
    command("/language ar")
    command("/reasoning")
    until(lambda: "اترك القرار للنموذج" in capture(), "Arabic reasoning picker did not open")
    screen = capture()
    (captures / "model-default-ar.txt").write_text(screen)
    assert "افتراضي" in row(screen, "high"), screen
    keys("Escape")
    command("/language en")
    until(lambda: "Ask anything" in capture(), "English did not come back")

    # A level picked by name goes out as it is.
    command("/reasoning low")
    until(lambda: "Reasoning effort: low" in capture(), "low was not chosen")
    third = send("third-low")
    assert third.get("reasoning_effort") == "low", third
    (captures / "final.txt").write_text(capture())
    print(
        "PASS real TUI: configured high shown and sent, provider default overrides it, a level picked is sent.",
        flush=True,
    )
finally:
    subprocess.run(["tmux", "kill-session", "-t", name], capture_output=True)
    if endpoint:
        subprocess.run(
            [str(binary), "--workspace", str(workspace), "serve", "--stop"],
            env=env,
            capture_output=True,
            timeout=30,
        )
    service.shutdown()
    service.server_close()
    e2e_scratch.settle(root)
