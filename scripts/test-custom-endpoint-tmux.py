#!/usr/bin/env python3
"""The terminal's Custom endpoint form against a fixture API served under `/openai/v1`, as Groq's is.

The address is typed the way people paste it: no scheme, no version, with the endpoint's own path. The
form checks it before saving, saves it where the API answers and says so, keeps the key typed with the
form out of the screen and the configuration, and a chat on the endpoint's model gets its answer.
Runs the real binary, daemon and TUI under tmux with an isolated profile.
"""

import json
import os
import pathlib
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from e2e_tmux import pane_command  # noqa: E402

KEY = "fixture-" + secrets.token_hex(6)
seen = []


class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, status, body, kind="application/json"):
        data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def authorized(self):
        return self.headers.get("Authorization") == "Bearer " + KEY

    def do_GET(self):
        seen.append(("GET", self.path, self.authorized()))
        if self.path != "/openai/v1/models":
            return self.send(404, {"error": {"message": "Unknown request URL"}})
        if not self.authorized():
            return self.send(401, {"error": {"message": "Invalid API Key"}})
        self.send(200, {"data": [{"id": "gateway-coder"}]})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        seen.append(("POST", self.path, self.authorized()))
        if self.path != "/openai/v1/chat/completions":
            return self.send(404, {"error": {"message": "Unknown request URL"}})
        if not self.authorized():
            return self.send(401, {"error": {"message": "Invalid API Key"}})
        assert body["model"] == "gateway-coder", body["model"]
        events = [
            {"choices": [{"delta": {"content": "custom-endpoint-proof"}}]},
            {"choices": [{"delta": {}, "finish_reason": "stop"}]},
        ]
        self.send(200, "".join(f"data: {json.dumps(event)}\n\n" for event in events) + "data: [DONE]\n\n", "text/event-stream")


def main():
    binary = pathlib.Path(os.environ.get("HAARNESS_BIN", "target/debug/haarness")).resolve()
    root = pathlib.Path(tempfile.mkdtemp(prefix="haarness-custom-endpoint-"))
    workspace = root / "ws"
    (root / "config").mkdir(parents=True)
    workspace.mkdir()
    # A developer machine's Ollama or LM Studio would count as connected.
    (root / "config/config.toml").write_text("[providers.ollama]\nenabled = false\n[providers.lmstudio]\nenabled = false\n")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    port = server.server_address[1]
    session = f"haarness-custom-{os.getpid()}"
    env = {key: value for key, value in os.environ.items() if not key.startswith("HAARNESS_")}
    env.update({"HAARNESS_DATA_DIR": str(root), "HAARNESS_SCHEDULE_SKIP_INSTALL": "1", "SSH_TTY": "fixture"})

    def pane():
        return subprocess.run(["tmux", "capture-pane", "-p", "-t", f"{session}:0.0"], capture_output=True, text=True).stdout

    def wait_for(label, *needles, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            text = pane()
            if all(needle in text for needle in needles):
                return text
            time.sleep(0.2)
        sys.stderr.write(pane() + "\n")
        raise SystemExit(label)

    def keys(*items, literal=False):
        subprocess.run(["tmux", "send-keys", *(["-l"] if literal else []), "-t", f"{session}:0.0", *items], check=True)
        time.sleep(0.25)

    def edit(value, secret=False):
        keys("Enter")
        keys(value, literal=True)
        if secret:
            assert value not in pane(), "the key form echoed the secret"
        keys("Enter")

    command = pane_command(env, [binary, "--workspace", workspace])
    subprocess.run(["tmux", "new-session", "-d", "-x", "120", "-y", "40", "-s", session, command], check=True)
    try:
        wait_for("The welcome picker did not open", "Welcome to Haarness")
        keys("Escape")
        keys("/providers", literal=True)
        keys("Enter")
        wait_for("The provider manager did not open", "Providers", "Add a custom endpoint")
        keys("Down", "Down", "Enter")
        wait_for(
            "The custom endpoint form did not open with its fields",
            "Custom endpoint",
            "Base URL",
            "API key (optional)",
            "Model IDs (optional)",
            "usually ending",
        )
        # Provider ID, then Display name (Scope stays global).
        edit("my-gateway")
        keys("Down", "Down")
        edit("My gateway")
        # Base URL, pasted as people do: no scheme, no version, the endpoint's path.
        keys("Down")
        edit(f"127.0.0.1:{port}/openai/chat/completions")
        # API key, after Protocol.
        keys("Down", "Down")
        edit(KEY, secret=True)
        assert KEY not in pane(), "the form showed the key"
        # Save, after Model IDs (left to discovery) and Requires authentication.
        keys("Down", "Down", "Down", "Enter")
        saved = wait_for(
            "Saving did not say where the address was saved",
            "Saved.",
            f"http://127.0.0.1:{port}/openai/v1",
        )
        assert "answers there" in saved, saved
        print("\n".join(line.strip() for line in saved.splitlines() if "Saved" in line or "answers there" in line))
        config = next(root.rglob("haarness.json"), None) or root / "config/config.toml"
        text = config.read_text()
        assert f"http://127.0.0.1:{port}/openai/v1\"" in text, text
        assert KEY not in text, "the key was written to the configuration"
        auth = next(root.rglob("auth.json"))
        assert json.loads(auth.read_text())["my-gateway"]["key"] == KEY
        assert ("GET", "/openai/v1/models", True) in seen, seen
        keys("Escape")
        # The endpoint's own model, discovered, answers a chat.
        keys("/models", literal=True)
        keys("Enter")
        wait_for("The model picker did not list the endpoint's model", "gateway-coder", timeout=60)
        keys("gateway-coder", literal=True)
        keys("Enter")
        keys("Say the proof word.", literal=True)
        keys("Enter")
        wait_for("The chat on the custom endpoint got no answer", "custom-endpoint-proof", timeout=60)
        assert ("POST", "/openai/v1/chat/completions", True) in seen, seen
        print("TUI custom endpoint: ok")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        server.shutdown()
        # The daemon the TUI started for the isolated profile, by the pid it recorded.
        for runtime in root.rglob("runtime*.json"):
            try:
                os.kill(int(json.loads(runtime.read_text())["pid"]), signal.SIGTERM)
            except (OSError, ValueError, KeyError):
                pass
        time.sleep(1)
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
