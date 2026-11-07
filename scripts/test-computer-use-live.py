#!/usr/bin/env python3
"""Computer use on this Mac's real screen, end to end: a built Haarness (with its embedded helper) serves an isolated
home, the deterministic test provider calls the mac_* tools, and a small lab app (computer-use-lab.swift) records
what they did. It checks looking, approvals and their scope, clicking, typing, Return, scrolling, dragging, the
password-field refusal, Escape stopping a turn mid-typing, the bar showing while the agent works, Pause on the bar
with Resume on the bar or in a client, turning computer use off, and quitting an app.

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


def api(path, body=None, headers=None):
    request = urllib.request.Request(connection["url"] + "/v1" + path,
                                     data=None if body is None else json.dumps(body).encode(),
                                     headers={"Authorization": "Bearer " + connection.get("token", ""),
                                              "Content-Type": "application/json", **(headers or {})})
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

    def turn(self, name, arguments, answer=None, expect_ask=None, headers=None, linger=None):
        """Runs one tool call. `answer` replies to an approval (once/always/reject); `expect_ask` says whether one
        must come. Returns (output, is_error, metadata, pending)."""
        text = "!tool " + name + " " + json.dumps(arguments)
        if linger:
            text = f"!linger {linger} {text}"
        turn = api("/threads/" + self.thread + "/turns", {"requestId": str(uuid.uuid4()), "text": text}, headers)
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

    def attachments(self):
        items = api("/threads/" + self.thread + "/items")
        items = items if isinstance(items, list) else items.get("items", [])
        return [path for item in items for path in item.get("data", {}).get("metadata", {}).get("attachments", [])]

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

    # With the password field focused, typing and printable keys aimed at the focus are refused too.
    output, error, _, _ = chat.turn("mac_click", {"ref": refs["password"]}, expect_ask=False)
    assert not error, output
    for name, arguments in (("mac_type", {"text": "hunter2"}), ("mac_press", {"key": "a"})):
        output, error, _, _ = chat.turn(name, arguments, expect_ask=False)
        assert error and "Password fields are refused" in output, (name, output)
    assert state().get("passwordLength") == 0, state()
    check("typing or a key into a focused password field is refused")

    # macOS may open its password suggestions (the Passwords app, a password manager) under the focused field, in
    # the app's own process: pointer input there is refused too. Escape closes them.
    output, error, metadata, _ = chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    password_field = re.search(r"\[text-field:secure-text-field\][^\n]* at=(\d+),(\d+) size=(\d+)x(\d+)", output)
    px, py, pw, ph = map(int, password_field.groups())
    lab_windows = subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--windows", str(lab_process.pid)],
                                 capture_output=True, text=True).stdout.strip()
    if int(lab_windows or 1) > 1:
        before_slider = state().get("slider")
        output, error, _, _ = chat.turn("mac_click", {"x": px + 30, "y": py + ph + 16}, expect_ask=False)
        assert error and "password suggestion menu" in output, output
        assert state().get("slider") == before_slider
        check("a click on macOS's password suggestions under the field is refused")
    output, error, _, _ = chat.turn("mac_press", {"key": "Escape"}, expect_ask=False)
    assert not error, output

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
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!linger 4000 !tool mac_snapshot " + json.dumps({"app": LAB})})
    until(lambda: int(subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--windows", helper],
                                     capture_output=True, text=True).stdout.strip() or 0) >= 1, "the bar shows", seconds=4)
    until(lambda: api("/turns/" + turn["id"])["status"] not in ("running", "queued"), "the lingering look ends")
    until(lambda: int(subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--windows", helper],
                                     capture_output=True, text=True).stdout.strip() or 0) == 0, "the bar goes with the turn", seconds=5)
    check("the bar is on screen while the agent works, and leaves with its turn")

    def bar(*arguments):
        result = subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--bar", helper, *arguments],
                                capture_output=True, text=True)
        return result if arguments else json.loads(result.stdout or "{}")

    # The bar speaks the language a client handed over, and keeps it for the server's next start.
    japanese = {"usingApp": "Haarness が %@ で作業中", "usingMac": "Haarness がこの Mac で作業中", "paused": "Haarness はあなたを待っています",
                "pause": "一時停止", "resume": "再開", "stop": "停止", "stopHint": "Esc でも止まります"}
    api("/manage/computer-use", {"action": "words", "bar": japanese})
    # The bar comes down when a turn ends, so it is read while one lingers after its look.
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!linger 6000 !tool mac_snapshot " + json.dumps({"app": LAB})})
    until(lambda: "Haarness が Haarness Lab で作業中" in bar().get("texts", []), "the bar speaks Japanese", seconds=6)
    until(lambda: api("/turns/" + turn["id"])["status"] not in ("running", "queued"), "the lingering look ends")
    assert json.loads((home / "data/computer-use/bar.json").read_text())["pause"] == "一時停止"
    check("the bar speaks the language a client sent, kept for the next start")

    # Pause on the bar: typing stops, the chat keeps the screen past the lease's idle time, Resume lets it go on.
    # Looks started straight through the API issued refs this script has not read, so it looks again first.
    chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    typed_before = len(state().get("name", ""))
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!tool mac_type " + json.dumps({"ref": refs["name"], "text": "y" * 3000})})
    until(lambda: len(state().get("name", "")) > typed_before + 20, "typing started before the pause")
    assert bar("press", "一時停止").returncode == 0
    time.sleep(1.5)
    held = len(state().get("name", ""))
    time.sleep(1.5)
    assert len(state().get("name", "")) == held, "typing went on while paused"
    other = Chat()
    time.sleep(31)
    output, error, _, _ = other.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    assert error and "The screen is busy" in output, output
    assert bar("press", "再開").returncode == 0
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the paused turn ends after Resume", seconds=60)
    assert finished["status"] != "interrupted", finished
    check(f"Pause on the bar holds the agent and the screen ({held - typed_before} characters before it), Resume lets go")

    # Pause on the bar, Resume in a client (what Desktop's paused line and /mac resume send): the status names the
    # paused chat, the bar hears the Resume and shows the agent at work again, and the typing goes on.
    chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    typed_before = len(state().get("name", ""))
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!tool mac_type " + json.dumps({"ref": refs["name"], "text": "x" * 1500})})
    until(lambda: len(state().get("name", "")) > typed_before + 20, "typing started before the pause")
    assert bar("press", "一時停止").returncode == 0
    until(lambda: api("/manage/computer-use", {"action": "get"})["data"].get("pausedChat") == chat.thread,
          "the status names the paused chat", seconds=5)
    held = len(state().get("name", ""))
    time.sleep(1)
    assert len(state().get("name", "")) == held, "typing went on while paused"
    assert "再開" in bar().get("buttons", []), bar()
    resumed = api("/manage/computer-use", {"action": "resume"})["data"]
    assert resumed["paused"] is False and resumed.get("pausedChat") is None, resumed
    until(lambda: "一時停止" in bar().get("buttons", []), "the bar shows the agent at work again", seconds=5)
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the turn resumed from a client ends", seconds=60)
    assert finished["status"] != "interrupted", finished
    assert len(state().get("name", "")) > held, "typing went on after Resume"
    check("Resume in a client lets a paused agent go on, and the bar shows it at work again")

    # Stop on the bar ends the turn using the screen.
    chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!tool mac_type " + json.dumps({"ref": refs["name"], "text": "z" * 3000})})
    until(lambda: "z" * 10 in state().get("name", ""), "typing started before Stop")
    assert bar("press", "停止").returncode == 0
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the stopped turn ends")
    assert finished["status"] == "interrupted", finished
    check("Stop on the bar ends the turn")
    api("/manage/computer-use", {"action": "words", "bar": {
        "usingApp": "Haarness is working in %@", "usingMac": "Haarness is working on this Mac", "paused": "Haarness is waiting for you",
        "pause": "Pause", "resume": "Resume", "stop": "Stop", "stopHint": "Esc stops it too"}})

    # Escape long after the agent's latest screen action belongs to the person: the turn goes on.
    look_started = time.monotonic()
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!linger 52000 !tool mac_snapshot " + json.dumps({"app": LAB})})
    until(lambda: api("/turns/" + turn["id"])["status"] == "running", "the lingering turn runs")
    time.sleep(max(0, 47 - (time.monotonic() - look_started)))
    subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--press-escape"], check=True)
    time.sleep(1)
    assert api("/turns/" + turn["id"])["status"] == "running", "Escape stopped a turn long after its last look"
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the lingering turn ends", seconds=30)
    assert finished["status"] == "completed", finished
    check("Escape 47 seconds after the latest screen action leaves the turn alone")

    # A child agent uses the apps its chat allowed: no new approval, and the screen is not busy for it.
    chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    count_before = state().get("count", 0)
    prompt = "!tool mac_click " + json.dumps({"ref": refs["count"], "snapshot": False})
    output, error, _, _ = chat.turn("task", {"agent": "general", "description": "Count once", "prompt": prompt}, expect_ask=False)
    assert not error, output
    until(lambda: state().get("count", 0) == count_before + 1, "the child agent's click counted")
    check("a child agent acts in an app its chat allowed without asking again")

    # A look at every app's windows asks; focusing an app that already runs asks nothing.
    output, error, metadata, _ = chat.turn("mac_snapshot", {"wholeScreen": True}, answer="once", expect_ask=True)
    assert not error and metadata.get("wholeScreen") is True, output
    output, error, _, _ = chat.turn("mac_apps", {"action": "activate", "app": LAB}, expect_ask=False)
    assert not error, output
    check("a whole-screen look asks, and focusing a running app does not")

    # An agent's own command cannot give agents more reach, even with its chat's variable cleared.
    output, _, _, _ = chat.turn("bash", {"command": f"env -u HAARNESS_SESSION_ID {binary} --workspace {workspace} mac allow com.apple.TextEdit"})
    assert "Only you can give agents more reach" in output, output
    assert "com.apple.TextEdit" not in settings_file.read_text()
    check("an agent's command is refused when it tries to allow an app")

    # A published chat's viewers see its transcript, so it never gets the screen, not even in the owner's turns.
    shared = Chat()
    output, error, _, _ = shared.turn("mac_snapshot", {"app": LAB}, headers={"x-haarness-remote-thread": shared.thread})
    assert error and "mac_snapshot" in output and not shared.attachments(), output
    output, error, _, _ = shared.turn("mac_snapshot", {"app": LAB})
    assert error, output
    check("a published chat never gets the screen tools")

    # Typing goes sixteen characters at a time, about 3 seconds per 4000, so the longest text the tool takes is typed
    # and Escape goes as soon as the first characters land.
    long_text = "x" * 10000
    chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    typed_before = len(state().get("name", ""))
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!tool mac_type " + json.dumps({"ref": refs["name"], "text": long_text})})
    until(lambda: len(state().get("name", "")) > typed_before, "typing started")
    subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--press-escape"], check=True)
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the turn ends")
    assert finished["status"] == "interrupted", finished
    time.sleep(0.5)
    typed = len(state().get("name", "")) - typed_before
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
