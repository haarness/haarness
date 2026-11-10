#!/usr/bin/env python3
"""Computer use on this Mac's real screen, end to end: a built Haarness (with its embedded helper) serves an isolated
home, the deterministic test provider calls the mac_* tools, and a small lab app (computer-use-lab.swift) records
what they did. It checks looking, approvals and their scope, clicking, typing, Return, scrolling, dragging, the
password-field refusal, Escape stopping a turn mid-typing, the bar showing while the agent works, Pause on the bar
with Resume on the bar or in a client, turning computer use off, and quitting an app.

A second part works behind the person's window: the lab copy runs as the person's app (`--person`), in front and
covering the lab, while a typist types into it as the person would. The agent looks at the covered lab, presses,
clicks a canvas by pixel, scrolls, types one line and several, uses a menu shortcut and Return, opens an app behind,
and is refused a password field; the person's app must stay in front with its key window, typing and pointer. It
also checks that the front is handed back, that a drag comes forward, that the agent's pointer shows, that the
display's awake hold comes and goes, that Escape stops the agent only while it works in the front app, and a
Command-letter shortcut. Reply fields the tool layer does not show are read from a helper the test starts itself.
Set HAARNESS_COMPUTER_LIVE_ELECTRON=<bundle id> of a running Electron app with a text field to add a row for it.

It moves the pointer and types into the lab windows only (the helper refuses an event whose spot belongs to another
app), but it does use the screen: run it while nobody needs the Mac.

    HAARNESS_COMPUTER_LIVE=1 python3 scripts/test-computer-use-live.py

macOS must allow Accessibility and Screen Recording for the app this runs in (the terminal app).
Set HAARNESS_COMPUTER_EVIDENCE=<dir> to keep the run's folder.
"""
import json
import os
import pathlib
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
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
person_file = root / "person-state.json"
LAB = "dev.haarness.test.lab"
PERSON = "dev.haarness.test.person"
LAB2 = "dev.haarness.test.lab2"
LSREGISTER = str(pathlib.Path("/System/Library/Frameworks/CoreServices.framework/Frameworks") / "LaunchServices.framework"
                 / "Support/lsregister")

env = clean_env()
env.update(HAARNESS_HOME=str(home), HAARNESS_TEST_PROVIDER="1")
checks = []
connection = {}
lab_process = None
person_process = None
direct = None
registered = []


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


def bundle(name, identifier, executable=None):
    """An app bundle around the lab's executable: the lab itself, the person's app, or a second lab to open."""
    app = root / (name + ".app")
    (app / "Contents/MacOS").mkdir(parents=True)
    (app / "Contents/Info.plist").write_text(f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundleExecutable</key><string>HaarnessLab</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
""")
    if executable:
        shutil.copy2(executable, app / "Contents/MacOS/HaarnessLab")
    return app


def build_lab():
    app = bundle("Haarness Lab", LAB)
    subprocess.run(["xcrun", "swiftc", "-O", str(repo / "scripts/computer-use-lab.swift"), "-o",
                    str(app / "Contents/MacOS/HaarnessLab")], check=True)
    return app


def person():
    try:
        return json.loads(person_file.read_text())
    except (OSError, ValueError):
        return {}


def windows_of(pid, sizes=False):
    out = subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--window-sizes" if sizes else "--windows", str(pid)],
                         capture_output=True, text=True).stdout.strip()
    return json.loads(out or "[]") if sizes else int(out or 0)


def awake_hold():
    return "Haarness is working on the screen" in subprocess.run(["pmset", "-g", "assertions"], capture_output=True,
                                                                 text=True).stdout


BEHIND_ELEMENTS = {
    "canvas": r'\[group\][^\n]*description="Canvas"',
    "notes": r'\[text-area\][^\n]*description="Notes"',
    "forward": r'\[button\] "Come forward"',
}


class Direct:
    """The helper the server unpacked, started by the test itself, for the reply fields the tool layer does not
    show (how an action was delivered, whether it reached the app, what a look says about the app behind)."""

    def __init__(self):
        found = sorted(home.rglob("haarness-mac"), key=lambda path: path.stat().st_mtime)
        assert found, "the server unpacked no helper under " + str(home)
        self.process = subprocess.Popen([str(found[-1])], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.lines = queue.Queue()
        self.events = []
        self.refs = {}
        self.frames = {}
        self.next = 0
        threading.Thread(target=self.read, daemon=True).start()
        self.ok("hello")

    def read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def call(self, op, seconds=60, **fields):
        self.next += 1
        self.process.stdin.write(json.dumps({"id": self.next, "op": op, **fields}) + "\n")
        self.process.stdin.flush()
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            try:
                line = self.lines.get(timeout=0.2)
            except queue.Empty:
                continue
            assert line is not None, "the helper exited"
            message = json.loads(line)
            if "event" in message:
                self.events.append(message["event"])
            elif message.get("id") == self.next:
                return message
        raise AssertionError("no answer to " + op)

    def ok(self, op, **fields):
        answer = self.call(op, **fields)
        assert answer["ok"], (op, answer)
        return answer["data"]

    def look(self, app=LAB, **fields):
        data = self.ok("snapshot", bundle=app, **fields)
        outline = data.get("outline", "")
        for key, pattern in {**LAB_ELEMENTS, **BEHIND_ELEMENTS}.items():
            match = re.search(r"@e(\d+) " + pattern + r"[^\n]* at=(\d+),(\d+) size=(\d+)x(\d+)", outline)
            if match:
                self.refs[key] = int(match.group(1))
                self.frames[key] = tuple(map(int, match.groups()[1:]))
        return data

    def middle(self, key):
        x, y, width, height = self.frames[key]
        return {"x": x + width / 2, "y": y + height / 2}

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()


def person_undisturbed(label):
    """The person's app is still in front with its key window, and their pointer never moved."""
    now = person()
    assert now.get("frontmost") == PERSON and now.get("active") and now.get("key"), (label, now)
    assert now.get("resigned") == 0 and now.get("keyLost") == 0, (label, now)
    assert not now.get("pointerMoved"), (label, now)


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


def switch_on():
    """Turns computer use on with the terms this binary asks for accepted, as the person's own agreement would."""
    version = json.loads(cli("mac", "status", "--json"))["terms"]["version"]
    settings_file.write_text(json.dumps({"enabled": True, "terms": {"version": version, "acceptedAt": "2026-11-07T00:00:00Z"}}))


try:
    lab = build_lab()
    switch_on()
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
    # paused chat, the bar hears the Resume and shows the agent at work again, and the agent is told it may go on.
    # The typing cut short by the pause does not pick up by itself: the person may have moved things meanwhile, so
    # the agent hears how far it got and looks again before its next step. The turn lingers after that answer, since
    # the bar comes down with its turn.
    chat.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    typed_before = len(state().get("name", ""))
    turn = api("/threads/" + chat.thread + "/turns",
               {"requestId": str(uuid.uuid4()),
                "text": "!linger 4000 !tool mac_type " + json.dumps({"ref": refs["name"], "text": "x" * 1500})})
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
    output, error, _ = chat.result(turn["id"], "mac_type")
    assert error and "Paused by the person after" in output and "running again" in output, output
    assert len(state().get("name", "")) == held, "the typing cut short by the pause went on by itself"
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
    # The press must have happened for the turn going on to mean anything.
    until(lambda: state().get("keys", [])[-1:] == ["\x1b"], "the Escape reached the lab", seconds=5)
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

    switch_on()
    output, error, _, pending = chat.turn("mac_apps", {"action": "quit", "app": LAB}, answer="once", expect_ask=True)
    assert not error, output
    assert pending["detail"]["__mac"]["summary"] == "Quit Haarness Lab"
    until(lambda: lab_process.poll() is not None, "the lab quits")
    check("quitting an app asks and quits it")

    # Behind the person's window. The lab opens first; the person's app opens over it and stays in front.
    lab_process = subprocess.Popen([str(lab / "Contents/MacOS/HaarnessLab"), str(state_file)])
    until(lambda: state().get("pid") == lab_process.pid, "the lab starts again")
    person_app = bundle("Haarness Person", PERSON, lab / "Contents/MacOS/HaarnessLab")
    person_process = subprocess.Popen([str(person_app / "Contents/MacOS/HaarnessLab"), "--person", str(person_file)])
    until(lambda: person().get("frontmost") == PERSON and person().get("active") and person().get("key"),
          "the person's app is in front")
    time.sleep(0.5)
    direct = Direct()
    data = direct.look()
    assert data.get("behind") is True and data.get("offscreen") is None, {key: data.get(key) for key in ("behind", "offscreen", "frontmost")}
    assert {"count", "name", "rows", "canvas", "notes", "forward"} <= set(direct.refs), direct.refs
    person_undisturbed("after the look")
    check("a look at the covered lab says it is behind, and leaves the person's app in front")

    # A search keeps the matching elements with what holds them; a close-up keeps the snapshot's coordinates.
    found = direct.ok("snapshot", bundle=LAB, find="Come forward")["outline"]
    assert '"Come forward"' in found and '"Count"' not in found and "[window]" in found, found
    x, y, width, height = direct.frames["canvas"]
    close = direct.ok("snapshot", bundle=LAB, zoom={"x": x, "y": y, "width": width, "height": height})
    assert close["zoom"]["scale"] > 1 and close["size"] == data["size"] and close.get("image"), {k: close.get(k) for k in ("zoom", "size")}
    assert f"at={x},{y} size={width}x{height}" in close["outline"], close["outline"][:600]
    direct.look()
    check("a look can search the outline, or zoom in with the snapshot's coordinates kept")

    # The person types in their app while the agent presses Count in the lab behind it.
    typist = subprocess.Popen([str(lab / "Contents/MacOS/HaarnessLab"), "--typist", "3", "abc "])
    until(lambda: len(person().get("text", "")) >= 3, "the person types")
    reply = direct.ok("click", ref=direct.refs["count"], expect=LAB)
    assert reply["via"] == "ax" and reply["background"] is True and reply["target"]["bundle"] == LAB, reply
    typist.wait(timeout=10)
    until(lambda: state().get("count") == 1, "the press counted")
    typed = person().get("text", "")
    assert len(typed) >= 20 and set(typed) <= set("abc "), typed
    assert not state().get("keys"), state().get("keys")
    person_undisturbed("after the press")
    check(f"a press in the covered lab while the person types: their {len(typed)} characters all stay in their app")

    # A pixel click on a view without controls, which only pointer events reach.
    reply = direct.ok("click", expect=LAB, **direct.middle("canvas"))
    assert reply["via"] == "process" and reply["background"] is True, reply
    until(lambda: state().get("canvasClicks") == 1, "the canvas was clicked")
    assert [22, 28] in windows_of(direct.process.pid, sizes=True), windows_of(direct.process.pid, sizes=True)
    person_undisturbed("after the pixel click")
    check("a pixel click lands on the covered lab's canvas, and the agent's pointer shows there")
    check("the agent's pointer is on screen at a background click")

    reply = direct.ok("scroll", ref=direct.refs["rows"], direction="down", amount=10, expect=LAB)
    assert reply["background"] is True, reply
    until(lambda: state().get("scrollY", 0) > 0, "the covered list scrolled")
    person_undisturbed("after scrolling")
    check(f"scrolling the covered lab ({reply['via']})")

    direct.look()
    reply = direct.ok("type", ref=direct.refs["name"], text="behind", expect=LAB)
    assert reply["via"] == "ax" and reply["background"] is True and reply.get("confirmed") is True, reply
    until(lambda: state().get("name", "").endswith("behind"), "one line went into Name")
    assert "behind" not in person().get("text", "")
    person_undisturbed("after one line")
    check("one line goes into the covered lab's Name field through Accessibility")

    count = state().get("count")
    reply = direct.ok("press", keys="Meta+b", expect=LAB)
    assert reply["via"] == "menu" and reply["background"] is True, reply
    until(lambda: state().get("count") == count + 10, "the menu shortcut reached the lab")
    person_undisturbed("after the menu shortcut")
    check("a menu shortcut reaches the covered lab through its menu item")

    reply = direct.ok("type", ref=direct.refs["notes"], text="line one\nline two", expect=LAB)
    assert reply["via"] == "process" and reply["background"] is True, reply
    until(lambda: state().get("notes") == "line one\nline two", "several lines went into Notes")
    assert "line" not in person().get("text", "")
    person_undisturbed("after several lines")
    check("several lines go into the covered lab's text view as keys to its process")

    # Through the server: text without a ref lands in the lab the agent looked at, not in the person's app in front.
    behind = Chat()
    output, error, _, _ = behind.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    assert not error, output
    output, error, _, _ = behind.turn("mac_type", {"text": "zq", "snapshot": False}, answer="always", expect_ask=True)
    assert not error, output
    until(lambda: "zq" in state().get("notes", ""), "the text went to the lab")
    assert "zq" not in person().get("text", ""), person().get("text")
    person_undisturbed("after typing without a ref")
    check("typing without a ref goes to the looked-at lab, not to the person's app in front")

    # Return reaches the covered lab's default button, and the agent's own Escape does not stop its turn.
    direct.look()
    direct.ok("click", ref=direct.refs["name"], expect=LAB)
    behind.turn("mac_snapshot", {"app": LAB}, expect_ask=False)
    output, error, _, _ = behind.turn("mac_press", {"key": "Enter", "snapshot": False}, expect_ask=False)
    assert not error, output
    until(lambda: state().get("submitted"), "Return pressed Submit behind the person's window")
    turn = api("/threads/" + behind.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!tool mac_press " + json.dumps({"key": "Escape", "snapshot": False})})
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the turn with the agent's Escape ends")
    assert finished["status"] == "completed", finished
    person_undisturbed("after Return and Escape")
    check("Return reaches the covered lab, and the agent's own Escape leaves its turn alone")

    # The person's Escape in their own app leaves an agent working behind them alone.
    escapes = person().get("escapes", 0)
    turn = api("/threads/" + behind.thread + "/turns",
               {"requestId": str(uuid.uuid4()), "text": "!linger 5000 !tool mac_snapshot " + json.dumps({"app": LAB})})
    until(lambda: api("/turns/" + turn["id"])["status"] == "running", "the lingering look runs")
    time.sleep(1)
    subprocess.run([str(lab / "Contents/MacOS/HaarnessLab"), "--press-escape"], check=True)
    until(lambda: person().get("escapes", 0) > escapes, "the person's Escape reached their app", seconds=5)
    finished = until(lambda: (value if (value := api("/turns/" + turn["id"]))["status"] not in ("running", "queued") else None),
                     "the lingering look ends", seconds=30)
    assert finished["status"] == "completed", finished
    check("the person's Escape in their own app does not stop an agent working behind them")

    # A password field in the covered lab is refused; the person's own password field blocks nothing there.
    direct.look()
    direct.ok("click", ref=direct.refs["password"], expect=LAB)
    refused = direct.call("type", text="hunter2", expect=LAB)
    assert not refused["ok"] and refused["code"] in ("password", "focus_unknown"), refused
    assert state().get("passwordLength") == 0, state()
    direct.ok("click", ref=direct.refs["name"], expect=LAB)
    (person_file.parent / (person_file.name + ".cmd")).write_text("secure")
    until(lambda: person().get("secureInput"), "the person's password field holds secure input", seconds=5)
    reply = direct.ok("type", ref=direct.refs["name"], text=" ok", expect=LAB)
    until(lambda: state().get("name", "").endswith(" ok"), "typing went on beside the person's password field")
    (person_file.parent / (person_file.name + ".cmd")).write_text("text")
    until(lambda: not person().get("secureInput"), "the person left their password field", seconds=5)
    check("a password field behind is refused, and the person's own password field blocks nothing in another app")

    # A button that brings the lab forward: the person gets the front again.
    reply = direct.ok("click", ref=direct.refs["forward"], expect=LAB)
    until(lambda: state().get("forwardPresses") == 1, "the button was pressed")
    until(lambda: person().get("frontmost") == PERSON and person().get("active"), "the person's app is in front again", seconds=5)
    check(f"an app that brings itself forward hands the front back (handed back: {reply.get('handedBack', False)})")

    # Opening an app starts it behind the person's window.
    lab2 = bundle("Haarness Lab Two", LAB2, lab / "Contents/MacOS/HaarnessLab")
    subprocess.run([LSREGISTER, "-f", str(lab2)], check=True)
    registered.append(lab2)
    reply = direct.ok("launch", bundle=LAB2)
    assert reply.get("background") is True and reply.get("bundle") == LAB2, reply
    assert windows_of(reply["pid"]) >= 1
    until(lambda: person().get("frontmost") == PERSON and person().get("active"), "the person's app keeps the front", seconds=5)
    direct.ok("quit", bundle=LAB2)
    check(f"opening an app starts it behind the person's window (handed back: {reply.get('handedBack', False)})")

    optional = os.environ.get("HAARNESS_COMPUTER_LIVE_ELECTRON")
    if optional:
        data = direct.look(optional)
        field = re.search(r"@e(\d+) \[text-field\]", data.get("outline", ""))
        assert data.get("behind") and field, data.get("outline", "")[:800]
        reply = direct.ok("type", ref=int(field.group(1)), text="from behind", expect=optional)
        assert reply["background"] is True, reply
        check(f"an Electron app covered by the person's window takes one line ({reply['via']})")

    # The display stays awake while requests flow, and the hold goes with the end of the turn.
    direct.look()
    assert awake_hold(), "no display hold while the agent works"
    direct.ok("release")
    until(lambda: not awake_hold(), "the display hold goes with the turn", seconds=10)
    check("the display is kept awake while the agent works, and let go when its turn ends")

    # A drag needs the real pointer: the lab comes forward for it.
    direct.look()
    x, y, width, height = direct.frames["level"]
    before = state().get("slider")
    # The fresh lab's slider stands at 10, near its left end.
    reply = direct.ok("drag", **{"from": {"x": x + width * 0.1, "y": y + height / 2}, "to": {"x": x + width * 0.9, "y": y + height / 2},
                                 "expect": LAB, "expectEnd": LAB})
    assert reply.get("fallback") == "drag" and reply["background"] is False, reply
    until(lambda: state().get("slider", 0) > before + 30, "the slider moved")
    until(lambda: state().get("active"), "the lab came forward")
    check("a drag brings the covered lab forward and works there")

    # A Command-letter shortcut through the keyboard, in front: the layout's Command keys.
    count = state().get("count")
    reply = direct.ok("press", keys="Meta+b", expect=LAB)
    assert reply["via"] == "pointer" and reply["background"] is False, reply
    until(lambda: state().get("count") == count + 10, "Command-B reached the lab in front")
    check("a Command-letter shortcut works in the app in front")
    print(f"PASS {len(checks)} live computer use checks", flush=True)
finally:
    if direct:
        direct.close()
    for app in registered:
        subprocess.run([LSREGISTER, "-u", str(app)], capture_output=True)
    if person_process and person_process.poll() is None:
        person_process.terminate()
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
