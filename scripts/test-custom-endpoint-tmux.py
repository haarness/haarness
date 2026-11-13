#!/usr/bin/env python3
"""The terminal's Custom endpoint form against a fixture API served under `/openai/v1`, as Groq's is.

The address is typed the way people paste it: no scheme, no version, with the endpoint's own path. The
form checks it before saving, saves it where the API answers and says so, keeps the key typed with the
form out of the screen and the configuration, and a chat on the endpoint's model gets its answer.

Then a provider added to project A with its key, and a project B (as a cloned repository would) that
defines the same id at a second fixture and makes it its model: that fixture is sent nothing, `haarness
run` fails before sending and says why, the TUI keeps a message for it and points to Connect, lists the
provider as not connected and says why where it is connected, and connecting it there saves the key
for that address too, after which a run gets its answer. With HAARNESS_UPGRADE_FROM naming an older Haarness binary, a key that
binary saved for a project's provider keeps working in that project after the update, and is not sent
to a project opened after it.
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
import e2e_env  # noqa: E402
from e2e_tmux import pane_command  # noqa: E402

KEY = "fixture-" + secrets.token_hex(6)
TEAM_KEY = "team-" + secrets.token_hex(6)
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
        self.server.keys.append(self.headers.get("Authorization"))
        return self.headers.get("Authorization") in ("Bearer " + KEY, "Bearer " + TEAM_KEY)

    def do_GET(self):
        self.server.seen.append(("GET", self.path, self.authorized()))
        if self.path != "/openai/v1/models":
            return self.send(404, {"error": {"message": "Unknown request URL"}})
        if not self.authorized():
            return self.send(401, {"error": {"message": "Invalid API Key"}})
        self.send(200, {"data": [{"id": "gateway-coder"}]})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        self.server.seen.append(("POST", self.path, self.authorized()))
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
    def fixture(requests):
        server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        server.seen, server.keys = requests, []
        threading.Thread(target=server.serve_forever, daemon=True).start()
        return server

    server = fixture(seen)
    port = server.server_address[1]
    # Project B's address for the same provider id.
    elsewhere = fixture([])
    port_b = elsewhere.server_address[1]
    session = f"haarness-custom-{os.getpid()}"
    env = e2e_env.clean_env()
    env.update({"HAARNESS_DATA_DIR": str(root), "SSH_TTY": "fixture"})

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

        # A provider added to project A with its key, through the CLI.
        def cli(*args, workspace=workspace, binary=binary):
            done = subprocess.run([binary, "--workspace", workspace, *args], env=env, capture_output=True, text=True, timeout=120)
            assert done.returncode == 0, (args, done.stdout, done.stderr)
            return done
        cli("provider", "add", "team-gateway", "--name", "Team gateway", "--url", f"http://127.0.0.1:{port}/openai/v1", "--project")
        cli("auth", "set", "team-gateway", "--key", TEAM_KEY)
        record = json.loads(auth.read_text())["team-gateway"]
        assert record["key"] == TEAM_KEY and record["addresses"] == [f"http://127.0.0.1:{port}/openai/v1"], record
        # Project B, as a cloned repository: the same id at another address.
        project_b = root / "ws-b"
        (project_b / ".haarness").mkdir(parents=True)
        (project_b / ".haarness/haarness.json").write_text(json.dumps({
            "model": "team-gateway/gateway-coder",
            "providers": {"team-gateway": {
                "name": "Team gateway", "base_url": f"http://127.0.0.1:{port_b}/openai/v1", "models": ["gateway-coder"]}}}))
        reason = f"the saved key is for http://127.0.0.1:{port}/openai/v1, so it was not sent to http://127.0.0.1:{port_b}/openai/v1."
        notice = reason[0].upper() + reason[1:] + " Connect to save a key for this address."

        def run(*args):
            return subprocess.run([binary, "--workspace", project_b, "run", *args], env=env, capture_output=True, text=True, timeout=180)

        # `haarness run` refuses before a chat exists when told the model, and the chat fails before
        # sending when the project's model is taken.
        named = run("-m", "team-gateway/gateway-coder", "Say the proof word.")
        assert named.returncode != 0 and f"Team gateway is not connected here: {reason}" in named.stderr, named
        print(" ".join(named.stderr.split()))
        taken = run("Say the proof word.")
        assert taken.returncode != 0 and notice in " ".join((taken.stdout + taken.stderr).split()), taken
        assert not elsewhere.seen, elsewhere.seen
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run(["tmux", "new-session", "-d", "-x", "120", "-y", "40", "-s", session, pane_command(env, [binary, "--workspace", project_b])], check=True)
        wait_for("Project B did not open", "Ask anything", timeout=60)
        # The TUI keeps the message and points to Connect, which says why.
        keys("Say the proof word.", literal=True)
        keys("Enter")
        wait_for("Sending did not point to Connect", "Team gateway is not connected · /connect team-gateway")
        assert not elsewhere.seen, elsewhere.seen
        keys("C-u")
        keys("/providers", literal=True)
        keys("Enter")
        wait_for("The provider manager did not open in project B", "Providers", "Type to filter")
        keys("team gateway", literal=True)
        listed = wait_for("The provider manager did not list the project's provider", "Providers", "Team gateway")
        row = next(line for line in listed.splitlines() if "Team gateway" in line)
        assert "Not connected" in row, row
        keys("Escape")
        assert TEAM_KEY not in json.dumps(elsewhere.keys), elsewhere.keys
        assert not any(request[0] == "GET" for request in elsewhere.seen), elsewhere.seen
        # Connect says why, then saves the key for this address too.
        keys("/connect team-gateway", literal=True)
        keys("Enter")
        why = " ".join(wait_for("Connect did not say why the saved key stayed home", "Connect Team gateway", "was not sent to").split())
        assert notice in why, why
        print(notice)
        assert TEAM_KEY not in json.dumps(elsewhere.keys), elsewhere.keys
        keys(TEAM_KEY, literal=True)
        keys("Enter")
        wait_for("Connecting the provider in project B did not say so", "Team gateway connected", timeout=60)
        record = json.loads(auth.read_text())["team-gateway"]
        assert record["addresses"] == [f"http://127.0.0.1:{port}/openai/v1", f"http://127.0.0.1:{port_b}/openai/v1"], record
        assert "Bearer " + TEAM_KEY in elsewhere.keys
        answered = run("Say the proof word.")
        assert answered.returncode == 0 and "custom-endpoint-proof" in answered.stdout, answered
        assert ("POST", "/openai/v1/chat/completions", True) in elsewhere.seen, elsewhere.seen
        print("TUI project provider keys: ok")

        upgrade_from = os.environ.get("HAARNESS_UPGRADE_FROM")
        if upgrade_from:
            # A profile the older Haarness made, before this one ever ran with it: a provider it added to
            # project C, with a key it saved with no address.
            old = pathlib.Path(upgrade_from).resolve()
            profile = root / "upgrade"
            (profile / "config").mkdir(parents=True)
            (profile / "config/config.toml").write_text((root / "config/config.toml").read_text())
            profile_env = dict(env, HAARNESS_DATA_DIR=str(profile))
            project_c, project_d = profile / "ws-c", profile / "ws-d"
            project_c.mkdir()

            def run(binary, workspace, *args):
                done = subprocess.run([binary, "--workspace", workspace, *args], env=profile_env, capture_output=True, text=True, timeout=120)
                assert done.returncode == 0, (args, done.stdout, done.stderr)
                return done

            run(old, project_c, "provider", "add", "legacy-gateway", "--url", f"http://127.0.0.1:{port}/openai/v1", "--project")
            run(old, project_c, "auth", "set", "legacy-gateway", "--key", KEY)
            saved = next(profile.rglob("auth.json"))
            assert "addresses" not in json.loads(saved.read_text())["legacy-gateway"]
            time.sleep(1)
            # After the update: the project it was added to still uses it.
            server.keys.clear()
            listed = run(binary, project_c, "providers").stdout
            assert "legacy-gateway\tlegacy-gateway\t1 model\tconnected" in listed, listed
            assert "Bearer " + KEY in server.keys, server.keys
            assert json.loads(saved.read_text())["legacy-gateway"]["addresses"] == [f"http://127.0.0.1:{port}/openai/v1"]
            # A project first opened after the update does not get it.
            (project_d / ".haarness").mkdir(parents=True)
            (project_d / ".haarness/haarness.json").write_text(json.dumps(
                {"providers": {"legacy-gateway": {"base_url": f"http://127.0.0.1:{port_b}/openai/v1"}}}))
            elsewhere.keys.clear()
            done = run(binary, project_d, "providers")
            assert "legacy-gateway\tlegacy-gateway\t0 models\tnot authenticated" in done.stdout, done.stdout
            assert f"was not sent to http://127.0.0.1:{port_b}" in done.stderr, done.stderr
            assert not any(elsewhere.keys), elsewhere.keys
            print("Upgrade from an older Haarness: ok")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        server.shutdown()
        elsewhere.shutdown()
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
