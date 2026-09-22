#!/usr/bin/env python3
"""Exercise explicit TUI languages, English default, persistence, and real chat."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import urllib.request
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sdks/tests"))
from server import Fixture, BINARY
ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    return subprocess.check_output(args, text=True)


def until(check, description):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(description)


server = Fixture()
session = f"haarness-language-{os.getpid()}"
evidence = []
try:
    server.start()
    def api(path, body=None):
        request = urllib.request.Request(server.url + "/v1" + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + server.token, "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)
    thread = api("/threads", {"title": "Language proof", "provider": "echo", "model": "echo-1"})
    tid = thread["id"]
    def pane():
        return run("tmux", "capture-pane", "-p", "-t", session)
    def attach(thread_id=tid):
        command = shlex.join(["env", "LANG=ja_JP.UTF-8", "LC_ALL=ja_JP.UTF-8",
            f"HOME={server.home}", f"HAARNESS_DATA_DIR={server.data}", "HAARNESS_TEST_PROVIDER=1",
            f"HAARNESS_SERVER_TOKEN={server.token}", str(BINARY), "attach", server.url, "--thread", thread_id])
        run("tmux", "new-session", "-d", "-x", "170", "-y", "45", "-s", session, command)
        until(lambda: "h a a r n e s s" in pane() or " child " in pane(), "TUI did not attach")
    def detach():
        run("tmux", "kill-session", "-t", session)
    def send(value):
        run("tmux", "send-keys", "-l", "-t", session, value)
        run("tmux", "send-keys", "-t", session, "Enter")
    def selected(code):
        expected = json.loads((ROOT / "locales" / f"{code}.json").read_text())["Ask anything…"]
        until(lambda: expected in pane(), f"{code} placeholder did not render")
    attach()
    selected("en")  # Japanese terminal environment must not select Japanese.
    evidence.append("ENGLISH DEFAULT UNDER JAPANESE LANG\n" + pane())
    detach()
    (server.data / "config/config.toml").write_text('[tui]\nlanguage="fr"\n')
    attach()
    selected("fr")  # An explicit config choice is still respected.
    for entry in json.loads((ROOT / "locales/languages.json").read_text()):
        code = entry["code"]
        send("/language " + code)
        selected(code)
        assert f'language = "{code}"' in (server.data / "config/tui-state.toml").read_text()
        evidence.append(code + "\n" + pane())
    send("/language zh-Hant")
    selected("zh-TW")
    detach()
    attach()
    selected("zh-TW")  # Saved choice beats the explicit French config.
    send("/language invalid")
    until(lambda: "/language" in pane(), "invalid-language feedback missing")
    selected("zh-TW")
    send("/language en")
    selected("en")
    send(" language-chat-proof")
    until(lambda: any(i['data']['type'] == 'assistant_message' and
        'language-chat-proof' in i['data'].get('text', '') for i in api(f'/threads/{tid}/items')),
        "real attached chat failed after language changes")
    detach()
    child = api("/threads", {"title": "Child language proof", "parentId": tid, "provider": "echo", "model": "echo-1"})
    attach(child['id'])
    until(lambda: ' child ' in pane(), "child badge is not English")
    assert "子エージェント" not in pane()
    evidence.append("ENGLISH CHILD BADGE\n" + pane())
    Path('/tmp/haarness-language-tmux.txt').write_text('\n\n'.join(evidence))
    print('PASS: English with Japanese LANG, all 17 languages, config and saved choice precedence, restart, invalid code, real chat and English child badge')
except BaseException:
    try:
        Path('/tmp/haarness-language-tmux-failure.txt').write_text(pane())
    except Exception:
        pass
    raise
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    server.close()
