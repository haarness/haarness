#!/usr/bin/env python3
"""Computer use on this Mac's real screen, end to end: a built Haarness (with its embedded helper) serves an isolated
home, the deterministic test provider calls the mac_* tools, and a small lab app (computer-use-lab.swift) records
what they did. It checks looking, approvals and their scope, clicking, typing, Return, scrolling, dragging, the
password-field refusal, Escape stopping a turn mid-typing, the bar showing while the agent works, turning computer use
off, and quitting an app.

It moves the pointer and types into the lab window only (the helper refuses an event whose spot belongs to another
app), but it does use the screen: run it while nobody needs the Mac.

    HAARNESS_COMPUTER_LIVE=1 python3 scripts/test-computer-use-live.py

macOS must allow Accessibility and Screen Recording for the app this runs in (the terminal app).
Set HAARNESS_COMPUTER_EVIDENCE=<dir> to keep the run's folder.
"""
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from e2e_env import clean_env  # noqa: E402

if sys.platform != "darwin":
    print("SKIP computer use is macOS only")
    sys.exit(0)
if os.environ.get("HAARNESS_COMPUTER_LIVE") != "1":
    print("SKIP set HAARNESS_COMPUTER_LIVE=1: this test uses the real screen")
    sys.exit(0)

repo = pathlib.Path(__file__).resolve().parent.parent
binary = str(pathlib.Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve())
root = pathlib.Path(tempfile.mkdtemp(prefix="haarness-computer-live-")).resolve()
workspace = root / "workspace"
(workspace / ".haarness").mkdir(parents=True)
(workspace / ".haarness/config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n')
home = root / "home"
(home / "config").mkdir(parents=True)
settings_file = home / "config/computer-use.json"
state_file = root / "lab-state.json"
LAB = "dev.haarness.test.lab"

env = clean_env()
env.update(HAARNESS_HOME=str(home), HAARNESS_TEST_PROVIDER="1")
checks = []
connection = {}
lab_process = None


def check(label):
    checks.append(label)
    print("PASS " + label, flush=True)


def cli(*args, input=None):
    return subprocess.run([binary, "--workspace", str(workspace), *args], input=input, env=env, text=True,
                          capture_output=True, check=True, timeout=60).stdout


def api(path, body=None):
    request = urllib.request.Request(connection["url"] + "/v1" + path,
                                     data=None if body is None else json.dumps(body).encode(),
                                     headers={"Authorization": "Bearer " + connection.get("token", ""),
                                              "Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=60) as reply:
        data = reply.read()
        return json.loads(data) if data else None


def until(fn, label, seconds=30):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = fn()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError("timed out: " + label)


def state():
    try:
        return json.loads(state_file.read_text())
    except (OSError, ValueError):
        return {}


def build_lab():
    app = root / "Haarness Lab.app"
    (app / "Contents/MacOS").mkdir(parents=True)
    (app / "Contents/Info.plist").write_text(f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{LAB}</string>
<key>CFBundleName</key><string>Haarness Lab</string>
<key>CFBundleExecutable</key><string>HaarnessLab</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
""")
    subprocess.run(["xcrun", "swiftc", "-O", str(repo / "scripts/computer-use-lab.swift"), "-o",
                    str(app / "Contents/MacOS/HaarnessLab")], check=True)
    return app


def outline_ref(output, pattern):
    match = re.search(r"@(e\d+) " + pattern, output)
    assert match, "no element matching " + pattern + " in\n" + output
    return match.group(1)


LAB_ELEMENTS = {
    "count": r'\[button\] "Count"',
    "name": r'\[text-field\] "[^"]*"[^\n]*(?:description|placeholder)="Name"',
    "password": r'\[text-field:secure-text-field\]',
    "level": r'\[slider\][^\n]*description="Level"',
    "rows": r'\[scroll-area\][^\n]*description="Rows"',
}
refs = {}


def read_refs(output):
    """Every snapshot issues new refs and retires the old ones, so the lab's refs are re-read from each outline."""
    if "elements (use these refs" in output:
        refs.update({key: outline_ref(output, pattern) for key, pattern in LAB_ELEMENTS.items()})
        first = re.search(r'@(e\d+) \[static-text\] "Row 1"', output)
        refs["row1"] = first.group(1) if first else None


class Chat:
    def __init__(self):
        self.thread = api("/threads", {"title": "Computer use live", "provider": "echo", "model": "echo-1"})["id"]

    def turn(self, name, arguments, answer=None, expect_ask=None):
        """Runs one tool call. `answer` replies to an approval (once/always/reject); `expect_ask` says whether one
        must come. Returns (output, is_error, metadata, pending)."""
        turn = api("/threads/" + self.thread + "/turns",
                   {"requestId": str(uuid.uuid4()), "text": "!tool " + name + " " + json.dumps(arguments)})
        pending = None
        end = time.monotonic() + 60
        while time.monotonic() < end:
            status = api("/turns/" + turn["id"])["status"]
            if status not in ("running", "queued"):
                break
            waiting = [item for item in api("/permissions") if item.get("threadId") == self.thread]
            if waiting:
                pending = waiting[0]
                assert answer, "an approval nobody expected: " + json.dumps(pending)[:400]
                api("/permissions/" + pending["permissionId"], {"decision": answer})
                answer = None
            time.sleep(0.1)
        if expect_ask is not None:
            assert (pending is not None) == expect_ask, f"{name}: asked={pending is not None}, expected {expect_ask}"
        result = self.result(turn["id"], name)
        read_refs(result[0])
        return result + (pending,)

    def result(self, turn_id, name):
        items = api("/threads/" + self.thread + "/items")
        items = items if isinstance(items, list) else items.get("items", [])
        for item in reversed(items):
            data = item.get("data", {})
            if data.get("type") == "tool_result" and data.get("name") == name and item.get("turnId") == turn_id:
                return data.get("output", ""), data.get("is_error", data.get("isError", False)), data.get("metadata", {})
        raise AssertionError("no result for " + name)


try:
    lab = build_lab()
    settings_file.write_text(json.dumps({"enabled": True, "terms": {"version": "1", "acceptedAt": "2026-11-07T00:00:00Z"}}))
    status = json.loads(cli("mac", "status", "--json"))
    assert status["supported"] and status["enabled"], status
    assert status["permissions"]["accessibility"] and status["permissions"]["screenRecording"], (
        "allow Accessibility and Screen Recording for " + str(status.get("host")))
    check("the embedded helper runs and macOS allows it")

    lab_process = subprocess.Popen([str(lab / "Contents/MacOS/HaarnessLab"), str(state_file)])
    until(lambda: state(), "lab starts")
    connection.update(json.loads(cli("desktop", input='{"action":"connect"}')))
    tools = json.dumps(api("/tools"))
    assert "mac_snapshot" in tools and "mac_click" in tools
    check("the mac_* tools are offered while computer use is on")

    chat = Chat()
    output, error, metadata, _ = chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    assert not error, output
    assert "Haarness Lab" in output and metadata.get("bundle") == LAB, output[:600]
    assert metadata.get("attachments") and pathlib.Path(metadata["attachments"][0]).stat().st_size > 1000
    assert set(LAB_ELEMENTS) <= set(refs) and refs["row1"], refs
    # The list opens at its top: the rows below its frame are counted, not placed over the window's other controls.
    assert '"Row 60"' not in output and "more elements are scrolled out of view here" in output, output
    check("a look at one app asks nothing and returns its elements and a screenshot")

    output, error, _, pending = chat.turn("mac_click", {"ref": refs["count"]}, answer="always", expect_ask=True)
    assert not error, output
    assert pending["permission"] == "computer" and pending["explicit"], pending
    mac = pending["detail"]["__mac"]
    assert mac["summary"] == 'Click button "Count" in Haarness Lab', mac["summary"]
    assert (mac.get("preview") or "").startswith("data:image/jpeg;base64,")
    until(lambda: state().get("count") == 1, "the click counted")
    check("the first action in the app asks, with a picture of the spot, and the click lands")

    output, error, _, _ = chat.turn("mac_click", {"ref": refs["count"], "snapshot": False}, expect_ask=False)
    assert not error, output
    until(lambda: state().get("count") == 2, "the second click counted")
    check("allowed in this chat: the next action asks nothing")

    output, error, _, _ = chat.turn("mac_type", {"ref": refs["name"], "text": "hello lab"}, expect_ask=False)
    assert not error, output
    until(lambda: state().get("name") == "hello lab", "typed text")
    check("typing into a field")

    output, error, _, _ = chat.turn("mac_press", {"key": "Enter"}, expect_ask=False)
    assert not error, output
    until(lambda: state().get("submitted"), "Return pressed the default button")
    check("pressing Return")

    output, error, _, _ = chat.turn("mac_type", {"ref": refs["password"], "text": "hunter2"}, expect_ask=False)
    assert error and "Password fields are refused" in output, output
    assert state().get("passwordLength") == 0
    check("a password field is refused before anything is typed")

    first_row = refs["row1"]
    output, error, _, _ = chat.turn("mac_scroll", {"ref": refs["rows"], "direction": "down", "amount": 10, "snapshot": False},
                                    expect_ask=False)
    assert not error, output
    until(lambda: state().get("scrollY", 0) > 0, "the list scrolled")
    check("scrolling")

    # Row 1 now sits above the list's frame, where Accessibility still places it: over the Name field.
    output, error, _, _ = chat.turn("mac_click", {"ref": first_row}, expect_ask=False)
    assert error and "scrolled out of view" in output, output
    assert state().get("name") == "hello lab", state()
    output, _, _, _ = chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    assert '"Row 1"' not in output and "scrolled out of view here" in output, output
    check("a row scrolled out of view is left out of the outline and refused as a target")

    knob = re.search(r"@" + refs["level"] + r" \[slider\][^\n]* at=(\d+),(\d+) size=(\d+)x(\d+)", output)
    assert knob, output
    x, y, width, height = map(int, knob.groups())
    before = state().get("slider")
    output, error, _, _ = chat.turn("mac_drag", {"from": {"x": x + width * 0.1, "y": y + height / 2},
                                                  "to": {"x": x + width * 0.9, "y": y + height / 2}}, expect_ask=False)
    assert not error, output
    until(lambda: state().get("slider", 0) > (before or 0) + 30, "the slider moved")
    check("dragging a slider")

    helper = subprocess.run(["pgrep", "-n", "-f", "mac-helper/.*/haarness-mac"], capture_output=True, text=True).stdout.strip()
    windows = subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--windows", helper], capture_output=True, text=True).stdout.strip()
    assert helper and int(windows or 0) >= 1, (helper, windows)
    check("the bar is on screen while the agent works")

    long_text = "x" * 4000
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!tool mac_type " + json.dumps({"ref": refs["name"], "text": long_text})})
    until(lambda: len(state().get("name", "")) > len("hello lab") + 20, "typing started")
    subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--press-escape"], check=True)
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the turn ends")
    assert finished["status"] == "interrupted", finished
    typed = len(state().get("name", "")) - len("hello lab")
    assert 0 < typed < len(long_text), typed
    check(f"Escape stops the turn and the typing under way ({typed} of {len(long_text)} characters typed)")

    cli("mac", "off")
    assert "mac_snapshot" not in json.dumps(api("/tools"))
    output, error, _, _ = chat.turn("mac_snapshot", {}, expect_ask=False)
    assert error, output
    check("turned off: the tools leave and a call is refused")

    settings_file.write_text(json.dumps({"enabled": True, "terms": {"version": "1", "acceptedAt": "2026-11-07T00:00:00Z"}}))
    output, error, _, pending = chat.turn("mac_apps", {"action": "quit", "app": LAB}, answer="once", expect_ask=True)
    assert not error, output
    assert pending["detail"]["__mac"]["summary"] == "Quit Haarness Lab"
    until(lambda: lab_process.poll() is not None, "the lab quits")
    check("quitting an app asks and quits it")
    print(f"PASS {len(checks)} live computer use checks", flush=True)
finally:
    if lab_process and lab_process.poll() is None:
        lab_process.terminate()
    if connection:
        try:
            cli("serve", "--stop")
        except Exception as error:  # noqa: BLE001
            print("could not stop the server:", error)
    evidence = os.environ.get("HAARNESS_COMPUTER_EVIDENCE")
    if evidence:
        shutil.copytree(root, pathlib.Path(evidence) / root.name, dirs_exist_ok=True, ignore=shutil.ignore_patterns("*.sock"))
    shutil.rmtree(root, ignore_errors=True)
