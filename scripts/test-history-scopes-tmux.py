#!/usr/bin/env python3
"""Real TUI recall across conversations, restarts and independent workspaces."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=90, **kwargs).stdout


def until(check, label):
    for _ in range(300):
        result = check()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError(label)


def main():
    root = Path(tempfile.mkdtemp(prefix="haarness-history-scopes-")).resolve()
    env = {**os.environ, "HAARNESS_HOME": str(root / "home"), "HAARNESS_DATA_DIR": str(root / "data"), "HAARNESS_TEST_PROVIDER": "1"}
    session = "haarness-history-" + str(uuid.uuid4())[:8]
    workspaces = []
    clients = []
    try:
        for index in range(2):
            workspace = root / f"workspace-{index}"
            workspaces.append(workspace)
            (workspace / ".haarness").mkdir(parents=True)
            (workspace / ".haarness/config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
            endpoint = json.loads(run([str(BINARY), "--workspace", str(workspace), "--trust-project", "desktop"], env=env, input='{"action":"connect"}'))

            def api(path, body=None, endpoint=endpoint):
                request = urllib.request.Request(endpoint["url"] + "/v1" + path, data=None if body is None else json.dumps(body).encode(), headers={"Authorization": "Bearer " + endpoint["token"], "Content-Type": "application/json"})
                with urllib.request.urlopen(request, timeout=10) as response:
                    return json.loads(response.read())
            clients.append(api)
        first = clients[0]("/threads", {"title": "First history fixture", "provider": "echo", "model": "echo-1"})["id"]
        second = clients[0]("/threads", {"title": "Second history fixture", "provider": "echo", "model": "echo-1"})["id"]
        other = clients[1]("/threads", {"title": "Other workspace fixture", "provider": "echo", "model": "echo-1"})["id"]

        def capture():
            return run(["tmux", "capture-pane", "-p", "-t", session])

        def launch(index, thread):
            subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
            command = shlex.join(["env", *[f"{key}={env[key]}" for key in ("HAARNESS_HOME", "HAARNESS_DATA_DIR", "HAARNESS_TEST_PROVIDER")], str(BINARY), "--workspace", str(workspaces[index]), "--trust-project", "--thread", thread])
            run(["tmux", "new-session", "-d", "-x", "140", "-y", "40", "-s", session, command])
            until(lambda: "review" in capture().lower(), "TUI did not open")

        def submit(index, thread, text=None, expected=None, recall=True):
            api = clients[index]
            def users():
                return [item["data"]["text"] for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "user_message"]
            count = len(users())
            if text:
                run(["tmux", "send-keys", "-l", "-t", session, text])
            elif recall:
                run(["tmux", "send-keys", "-t", session, "Up"])
            run(["tmux", "send-keys", "-t", session, "Enter"])
            until(lambda: len(users()) > count, "recalled prompt was not submitted")
            assert users()[-1] == (expected or text), users()
            until(lambda: not api("/turns/active"), "turn did not finish")
            return capture()

        launch(0, first)
        submit(0, first, "first-conversation-marker")
        launch(0, second)
        submit(0, second, "second-conversation-marker")
        launch(0, first)
        submit(0, first, expected="first-conversation-marker")
        launch(1, other)
        run(["tmux", "send-keys", "-t", session, "Up", "Enter"])
        time.sleep(.3)
        assert clients[1](f"/threads/{other}/items") == [], "history crossed workspaces"
        submit(1, other, "other-workspace-marker")
        launch(0, first)
        proof = submit(0, first, expected="first-conversation-marker")
        for prompt in ["git status", "npm test", "git diff"]:
            submit(0, first, prompt)
        run(["tmux", "send-keys", "-l", "-t", session, "git "])
        run(["tmux", "send-keys", "-t", session, "Up", "Up"])
        submit(0, first, expected="git status", recall=False)
        run(["tmux", "send-keys", "-l", "-t", session, "git "])
        run(["tmux", "send-keys", "-t", session, "Up", "Down"])
        # Inspect and submit the restored draft, including its trailing space.
        until(lambda: "git " in capture(), "prefix draft was not restored")
        submit(0, first, expected="git ", recall=False)
        run(["tmux", "send-keys", "-l", "-t", session, "unmatched draft"])
        run(["tmux", "send-keys", "-t", session, "Up"])
        proof = submit(0, first, expected="unmatched draft", recall=False)
        (root / "history-tui.txt").write_text(proof)
        print(json.dumps({"passed": ["conversation-first recall", "TUI restart", "workspace isolation", "server switching", "prefix filtering", "prefix draft restoration", "unmatched draft preservation"], "fixture": str(root)}, indent=2))
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        for workspace in workspaces:
            try:
                run([str(BINARY), "--workspace", str(workspace), "serve", "--stop"], env=env)
            except Exception:
                pass


if __name__ == "__main__":
    main()
