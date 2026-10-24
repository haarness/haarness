"""Private, owned server fixtures shared by SDK tests and schema export."""

import json
import os
from pathlib import Path
from contextlib import contextmanager
import queue
import re
import secrets
import subprocess
import tempfile
import threading
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()


class Fixture:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="haarness-sdk-e2e-")
        self.root = Path(self.tmp.name).resolve()
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        self.data = self.root / "state"
        self.home = self.root / "home"
        self.home.mkdir()
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "HAARNESS_DATA_DIR": str(self.data),
            "HAARNESS_TEST_PROVIDER": "1",
            # A test server never reports usage statistics (a release build
            # would send them to Haarness Cloud a minute after it starts).
            "HAARNESS_TELEMETRY": "0",
        }
        # Use explicit echo defaults and an ask rule to test approval round-trips.
        config = self.workspace / ".haarness"
        config.mkdir()
        (config / "config.toml").write_text(
            '[defaults]\nprovider="echo"\nmodel="echo-1"\n\n[permissions]\nbash="ask"\n'
        )
        (config / "commands").mkdir()
        (config / "commands" / "greet.md").write_text(
            "---\ndescription: SDK fixture greeting\n---\nHello $ARGUMENTS\n"
        )
        skill = config / "skills" / "fixture"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text(
            "---\nname: fixture\ndescription: SDK fixture skill\n---\nReturn fixture evidence.\n"
        )
        workflow = config / "workflows"
        workflow.mkdir()
        (workflow / "fixture.js").write_text(
            'export const meta = { name: "fixture", phases: ["run"] };\nphase("run"); return {ok: true};\n'
        )
        self.plugin = self.root / "plugin"
        (self.plugin / ".claude-plugin").mkdir(parents=True)
        (self.plugin / ".claude-plugin" / "plugin.json").write_text(
            json.dumps(
                {
                    "name": "sdk-fixture",
                    "version": "1.0.0",
                    "description": "SDK fixture",
                }
            )
        )
        self.market = self.root / "market"
        (self.market / ".claude-plugin").mkdir(parents=True)
        (self.market / ".claude-plugin" / "marketplace.json").write_text(
            json.dumps(
                {"name": "sdk-market", "owner": {"name": "Fixture"}, "plugins": []}
            )
        )
        (self.workspace / "fixture.txt").write_text("before\n")
        for args in (
            ["init", "-b", "main"],
            ["add", "."],
            [
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-m",
                "Initial fixture",
            ],
        ):
            subprocess.run(
                ["git", *args], cwd=self.workspace, check=True, capture_output=True
            )
        self.token = secrets.token_hex(24)
        self.env["HAARNESS_SERVER_TOKEN"] = self.token
        self.process = None

    def start(self):
        ready = queue.Queue()
        self.log = (self.root / "server.log").open("w")
        self.process = subprocess.Popen(
            [
                str(BINARY),
                "--workspace",
                str(self.workspace),
                "--trust-project",
                "serve",
                "--port",
                "0",
            ],
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=self.log,
            text=True,
        )

        def read():
            for line in self.process.stdout:
                match = re.search(r"Haarness server listening on (http://\S+)", line)
                if match:
                    ready.put(match[1])
            ready.put(None)

        threading.Thread(target=read, daemon=True).start()
        self.url = ready.get(timeout=60)
        if not self.url:
            raise RuntimeError(
                "Fixture server startup failed: "
                + (self.root / "server.log").read_text()[-2000:]
            )
        return self

    def json(self, path):
        with urllib.request.urlopen(
            urllib.request.Request(
                self.url + path, headers={"Authorization": "Bearer " + self.token}
            ),
            timeout=30,
        ) as response:
            return json.load(response)

    @contextmanager
    def browser_host(self, thread_id):
        """Protocol fixture for SDK serialization; Electron tests own Chromium proof."""
        from websockets.sync.client import connect
        from websockets.exceptions import ConnectionClosed

        url = self.url.replace("http://", "ws://", 1)
        errors = []
        calls = []
        with connect(
            f"{url}/v1/threads/{thread_id}/browser/host?version=2",
            additional_headers={"Authorization": "Bearer " + self.token},
        ) as socket:
            assert json.loads(socket.recv(timeout=10))["type"] == "attached"

            def respond():
                try:
                    for raw in socket:
                        message = json.loads(raw)
                        assert message["type"] == "command", message
                        command = message["command"]
                        assert command["action"]["type"] == "tabs.list", command
                        calls.append(command)
                        value = (
                            {"resources": [], "key": "sdk-inventory"}
                            if command.get("inspect")
                            else {"tabs": [], "focusedTabID": None}
                        )
                        socket.send(json.dumps({
                            "type": "result", "id": message["id"],
                            "outcome": {"type": "success", "result": {
                                "value": value, "files": [],
                            }},
                        }))
                except ConnectionClosed:
                    pass
                except BaseException as error:
                    errors.append(error)

            worker = threading.Thread(target=respond, daemon=True)
            worker.start()
            try:
                yield calls
            finally:
                socket.close()
                worker.join(timeout=5)
                assert not worker.is_alive(), "Browser fixture did not close"
                assert not errors, errors

    @contextmanager
    def extension_host(self, thread_id, tab_id):
        """Haarness for Chrome's side of the protocol: one profile holding one
        tab of `thread_id`, answering tab lists. The extension owns Chrome proof."""
        from websockets.sync.client import connect
        from websockets.exceptions import ConnectionClosed
        import uuid

        url = self.url.replace("http://", "ws://", 1)
        errors = []
        calls = []
        tab = {"id": tab_id, "url": "https://example.test/", "title": "Example",
               "loading": False, "canGoBack": False, "canGoForward": False, "generation": 1}
        with connect(
            f"{url}/v1/browser/extension/host?version=1",
            additional_headers={"Authorization": "Bearer " + self.token},
        ) as socket:
            attached = json.loads(socket.recv(timeout=10))
            assert attached["type"] == "attached", attached
            assert attached["server"]["workspace"] == str(self.workspace), attached
            socket.send(json.dumps({
                "type": "hello",
                "browser": {"id": str(uuid.uuid4()), "kind": "chrome", "name": "Google Chrome",
                            "label": "SDK", "version": "154.0.8037.98"},
                "extension": {"version": "0.1.1"},
                "paused": False, "stopped": False, "focusedAt": 1,
            }))
            socket.send(json.dumps({
                "type": "state", "thread": thread_id,
                "state": {"tabs": [tab], "focusedTabID": tab_id},
            }))
            # Messages are handled in order: the pong means the tab is known.
            socket.send(json.dumps({"type": "ping"}))
            assert json.loads(socket.recv(timeout=10))["type"] == "pong"

            def respond():
                try:
                    for raw in socket:
                        message = json.loads(raw)
                        if message["type"] != "command":
                            continue
                        command = message["command"]
                        assert message["thread"] == thread_id, message
                        assert command["action"]["type"] == "tabs.list", command
                        calls.append(command)
                        value = (
                            {"resources": [], "key": "inventory"}
                            if command.get("inspect")
                            else {"tabs": [tab], "focusedTabID": tab_id}
                        )
                        socket.send(json.dumps({
                            "type": "result", "id": message["id"],
                            "outcome": {"type": "success", "result": {
                                "value": value, "files": [],
                            }},
                        }))
                except ConnectionClosed:
                    pass
                except BaseException as error:
                    errors.append(error)

            worker = threading.Thread(target=respond, daemon=True)
            worker.start()
            try:
                yield calls
            finally:
                socket.close()
                worker.join(timeout=5)
                assert not worker.is_alive(), "Extension fixture did not close"
                assert not errors, errors

    def close(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)
        if self.process and self.process.stdout:
            self.process.stdout.close()
        if hasattr(self, "log"):
            self.log.close()
        self.tmp.cleanup()

    def __enter__(self):
        try:
            return self.start()
        except BaseException:
            self.close()
            raise

    def __exit__(self, *args):
        self.close()


if __name__ == "__main__":
    import sys

    with Fixture() as server:
        schema = server.json("/openapi.json")
        # The server fills its document in from the generated openapi.json
        # and lists what it filled; schema.json is the server's own part.
        filled = schema.pop("x-haarness-reference", {})
        for path in filled.get("paths", []):
            del schema["paths"][path]
        for name in filled.get("schemas", []):
            del schema["components"]["schemas"][name]
        target = ROOT / "sdks/schema.json"
        if "--check" in sys.argv:
            assert json.loads(target.read_text()) == schema, (
                "Live server schema differs from SDK schema snapshot"
            )
        else:
            target.write_text(json.dumps(schema, indent=2, sort_keys=True) + "\n")
        print(
            "Live server schema verified"
            if "--check" in sys.argv
            else "Schema exported"
        )
