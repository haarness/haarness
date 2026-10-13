#!/usr/bin/env python3
"""Opt-in real native connector + Cloud + Python/TS SDK lifecycle/stress test.

HAARNESS_REMOTE_E2E=1 HAARNESS_CLOUD_API_URL=http://127.0.0.1:19080
HAARNESS_API_KEY must allow remote:read, remote:connect, remote:control and
workspaces:read (the attention list).
Uses the built CLI, isolated local files/state, echo provider, and owned registrations.
Connectors reach Cloud through a loopback proxy the test can cut, as a network
change would, while the connector keeps running.

Push notifications are checked too when Cloud writes them to files
(HAARNESS_CLOUD_PUSH_PROVIDER=file): set HAARNESS_PUSH_DIRECTORY to that
directory and HAARNESS_PUSH_BEARER to a native app access token of the key's
user (only such a sign-in can register a phone). The test registers a fake
device for the run and removes it at the end.
The browser/model proof is a separate pass; this test never prints credentials.
"""

import asyncio, base64, hashlib, json, os, secrets, shutil, subprocess, threading
import time, uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import socket as net  # the PTY proof below names its connection `socket`
import httpx
from haarness_sdk import Haarness, HaarnessCloud, AsyncHaarnessCloud, ApiError

assert os.environ.get("HAARNESS_REMOTE_E2E") == "1"
BASE = os.environ["HAARNESS_CLOUD_API_URL"].rstrip("/")
assert httpx.URL(BASE).host in ("127.0.0.1", "localhost"), (
    "Only a local test Cloud is allowed"
)
REPO = Path(__file__).resolve().parents[1]
BIN = str(
    Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()
)
ROOT = REPO / "target" / ("remote-e2e-" + uuid.uuid4().hex[:8])
ROOT.mkdir(parents=True)
CONFIG = '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\nwrite="ask"\n[lsp]\nenabled=false\n[lazy_tools]\nmode="off"\n'


def project_folder(name):
    folder = ROOT / name
    (folder / ".haarness").mkdir(parents=True)
    (folder / ".haarness/config.toml").write_text(CONFIG)
    subprocess.run(["git", "init", "-q", str(folder)], check=True)
    return folder


class Proxy:
    """Loopback TCP proxy in front of Cloud. `cut` drops every connection and
    refuses new ones, as a network change or a Cloud restart does, while the
    processes behind it keep running; `resume` lets them back in."""

    def __init__(self, upstream):
        self.upstream = (upstream.host, upstream.port)
        self.lock = threading.Lock()
        self.open = set()
        probe = net.socket()
        probe.bind(("127.0.0.1", 0))
        self.port = probe.getsockname()[1]
        probe.close()
        self.url = f"http://127.0.0.1:{self.port}"
        self.listener = None
        self.resume()

    def resume(self):
        listener = net.socket()
        listener.setsockopt(net.SOL_SOCKET, net.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", self.port))
        listener.listen(128)
        self.listener = listener
        threading.Thread(target=self._accept, args=(listener,), daemon=True).start()

    def _accept(self, listener):
        while True:
            try:
                client, _ = listener.accept()
            except OSError:
                return
            try:
                server = net.create_connection(self.upstream, timeout=10)
                server.settimeout(None)
            except OSError:
                client.close()
                continue
            with self.lock:
                self.open.update((client, server))
            for source, sink in ((client, server), (server, client)):
                threading.Thread(
                    target=self._pipe, args=(source, sink), daemon=True
                ).start()

    def _pipe(self, source, sink):
        try:
            while data := source.recv(65536):
                sink.sendall(data)
        except OSError:
            pass
        finally:
            for end in (source, sink):
                self._drop(end)

    def _drop(self, end):
        with self.lock:
            self.open.discard(end)
        try:
            end.shutdown(net.SHUT_RDWR)
        except OSError:
            pass
        end.close()

    def cut(self):
        self.listener.close()
        self.listener = None
        with self.lock:
            ends = list(self.open)
        for end in ends:
            self._drop(end)


proxy = Proxy(httpx.URL(BASE))
WORK = project_folder("workspace")
(WORK / ".hidden-proof").write_text("remote-hidden-proof")
env = {
    k: os.environ[k]
    for k in [
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "TMPDIR",
        "HAARNESS_API_KEY",
    ]
    if k in os.environ
}
env.update(
    HAARNESS_CLOUD_API_URL=proxy.url,
    HAARNESS_DATA_DIR=str(ROOT / "runtime"),
    HAARNESS_CLOUD_CONFIG_DIR=str(ROOT / "cloud"),
    HAARNESS_TEST_PROVIDER="1",
    HAARNESS_LOG="warn",
)
args = [BIN, "--workspace", str(WORK)]
cloud = HaarnessCloud(BASE, token=env["HAARNESS_API_KEY"])
hosts = []
procs = []
checks = []
local = None
extra = None
PUSH_DIRECTORY = os.environ.get("HAARNESS_PUSH_DIRECTORY")
PUSH_BEARER = os.environ.get("HAARNESS_PUSH_BEARER")
phone = None
pushes = None


def proof(name):
    checks.append(name)
    print("PASS:", name, flush=True)


def cli(*command):
    p = subprocess.run(
        args + list(command), env=env, text=True, capture_output=True, timeout=120
    )
    assert p.returncode == 0, (command, p.stderr)
    return p.stdout


def until(fn, label, seconds=30):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            value = fn()
        except ApiError as error:
            if error.status != 429:
                raise
            value = None
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(label)


def deny(fn, status=403):
    try:
        fn()
    except ApiError as error:
        assert error.status == status, (error.status, status, str(error))
    else:
        raise AssertionError("expected denial")


def connector(chat=None):
    name = "Remote stress " + ROOT.name + (" shared" if chat else "")
    log = (ROOT / ("connector-shared.log" if chat else "connector.log")).open("a")
    p = subprocess.Popen(
        args + ["remote", "run", "--name", name] + (["--chat", chat] if chat else []),
        env=env,
        stdout=log,
        stderr=log,
    )
    procs.append(p)

    def ready():
        assert p.poll() is None, "Connector exited; inspect " + str(ROOT)
        return next(
            (
                h
                for h in cloud.list_remote_hosts()["items"]
                if h["name"] == name and h["connected"]
            ),
            None,
        )

    host = until(ready, "connector did not connect")
    hosts.append(host["id"]) if host["id"] not in hosts else None
    return p, host


def stop(p):
    p.terminate()
    p.wait(15)


def attention(**match):
    """The account's open attention rows whose fields equal `match`."""
    rows, cursor = [], None
    while True:
        page = cloud.list_attention(
            {"limit": 100, **({"cursor": cursor} if cursor else {})}
        )
        rows += page["items"]
        cursor = page.get("nextCursor")
        if not cursor:
            return [r for r in rows if all(r.get(k) == v for k, v in match.items())]


def one(**match):
    """The single open row matching, or None; two would be a duplicate."""
    rows = attention(**match)
    assert len(rows) <= 1, ("duplicate attention rows", match, rows)
    return rows[0] if rows else None


def ask(runtime, thread, command):
    """Start a turn that asks to run `command` (bash asks in these projects)."""
    return runtime.start_turn(
        thread,
        {
            "requestId": str(uuid.uuid4()),
            "text": "!tool bash " + json.dumps({"command": command}),
        },
    )


def pending(runtime, thread, questions=False):
    """The approval (or question) waiting in `thread` or in its helper agents."""

    def find():
        waiting = runtime.list_questions() if questions else runtime.list_permissions()
        return next(
            (p for p in waiting if thread in (p["threadId"], p.get("rootThreadId"))),
            None,
        )

    return until(find, "nothing waits in " + thread)


def chat(runtime, title):
    return runtime.create_thread(
        {"title": title, "provider": "echo", "model": "echo-1"}
    )


def reply(runtime, request):
    runtime.reply_permission(request["permissionId"], {"decision": "once"})


def completed(turn, runtime=None):
    waited = (runtime or local).wait_turn(turn["id"], {"timeout_ms": 20000})
    assert waited["status"] == "completed", waited


def finished_rows(turn):
    return attention(turnId=turn, kind="turn_completed") + attention(
        turnId=turn, kind="turn_failed"
    )


class Presence(threading.Thread):
    """Watches one open row, several times a second, until stopped: a snapshot
    that resolved it and a later one that reopened it would show as a gap."""

    def __init__(self, request):
        super().__init__(daemon=True)
        self.request, self.samples, self.gaps = request, [], []
        self.done = threading.Event()

    def run(self):
        while not self.done.wait(0.2):
            try:
                rows = attention(itemId=self.request)
            except ApiError as error:
                if error.status == 429:
                    continue
                self.gaps.append(str(error))
                continue
            self.samples.append(rows[0]["connected"] if len(rows) == 1 else None)
            if len(rows) != 1:
                self.gaps.append(len(rows))

    def stop(self):
        self.done.set()
        self.join(10)
        return self


class Events(threading.Thread):
    """The account event stream, recorded for the run."""

    def __init__(self):
        super().__init__(daemon=True)
        self.client = HaarnessCloud(BASE, token=env["HAARNESS_API_KEY"])
        self.frames = []

    def run(self):
        try:
            for frame in self.client.events(reconnect=True):
                self.frames.append(frame)
        except Exception:
            pass

    def attention(self, source):
        return [
            f
            for f in self.frames
            if f.get("entity") == "attention" and f.get("id") == source
        ]


class Pushes:
    """What the file push provider wrote for this account since the run began."""

    def __init__(self, directory, account):
        self.directory, self.account = Path(directory), account
        self.before = {p.name for p in self.directory.glob("*.apns")}
        self.read = {}

    def payloads(self):
        for path in self.directory.glob("*.apns"):
            if path.name in self.before or path.name in self.read:
                continue
            try:
                self.read[path.name] = json.loads(path.read_text())
            except (OSError, ValueError):
                continue  # still being written
        return list(self.read.values())

    def alerts(self, item):
        return [
            p
            for p in self.payloads()
            if p.get("haarness", {}).get("accountId") == self.account
            and p["haarness"].get("itemId") == item
            and "alert" in p.get("aps", {})
        ]

    def evicts(self, attention_ids):
        return [
            p
            for p in self.payloads()
            if p.get("haarness", {}).get("kind") == "evict"
            and set(p["haarness"].get("clear", [])) & set(attention_ids)
        ]


# Notifications are counted at the end, once the dispatcher has provably run
# again (a later request's alert and its withdrawal): a second alert for any
# of these would have been sent by then. A loaded Cloud runs the dispatcher
# late, so the first alert may take a while.
announced = {}
PUSH_WAIT = 120


def alerted(item, label):
    """The first notification announcing `item`, to be counted again at the end."""
    announced[item] = label
    return until(
        lambda: pushes.alerts(item), "no notification for " + label, PUSH_WAIT
    )[0]


def withdrawn(rows, label):
    return until(lambda: pushes.evicts(rows), label + " was not withdrawn", PUSH_WAIT)


events = Events()
events.start()
try:
    endpoint = json.loads(
        subprocess.run(
            args + ["desktop"],
            env=env,
            input='{"action":"connect"}',
            text=True,
            capture_output=True,
            check=True,
            timeout=120,
        ).stdout
    )
    local = Haarness(endpoint["url"], token=endpoint["token"])
    process, record = connector()
    hid = record["id"]
    host = cloud.remote_host(hid)
    project = next(
        p for p in host.host_projects()["workspaces"] if p["path"] == str(WORK)
    )
    pid = project["id"]
    runtime = host.workspace(pid)
    assert any(
        d["name"] == ".haarness"
        for d in host.host_directories({"hidden": True})["directories"]
    )
    assert any(
        f["path"] == ".hidden-proof"
        for f in runtime.list_files({"hidden": True})["entries"]
    )
    proof("native host discovery, directory browsing and hidden files")
    if PUSH_DIRECTORY and PUSH_BEARER:
        phone = HaarnessCloud(BASE, token=PUSH_BEARER)
        phone.register_push_device(
            {
                "token": secrets.token_hex(32),
                "environment": "sandbox",
                "bundleId": "dev.haarness.ios",
                "deviceLabel": "Remote e2e " + ROOT.name,
            }
        )
        preferences = phone.get_push_preferences()
        assert preferences["available"], preferences
        assert all(
            c["enabled"]
            for c in preferences["categories"]
            if c["category"] in ("approvals", "completions")
        ), preferences
        pushes = Pushes(PUSH_DIRECTORY, record["accountId"])
    else:
        print(
            "SKIP: push notifications (set HAARNESS_PUSH_DIRECTORY and HAARNESS_PUSH_BEARER)",
            flush=True,
        )

    # What waits on the person reaches Cloud: ids, the category and the chat's
    # title, never the command, and only for as long as it waits.
    secret = "attention-secret-" + uuid.uuid4().hex
    root = chat(runtime, "Attention root")
    turn = ask(runtime, root["id"], f"printf {secret} > attention-proof.txt")
    request = pending(runtime, root["id"])
    row = until(
        lambda: one(itemId=request["permissionId"]),
        "the approval did not reach the attention list",
    )
    expected = {
        "source": "host",
        "hostId": hid,
        "hostName": record["name"],
        "kind": "permission",
        "projectId": pid,
        "projectName": project["name"],
        "threadId": root["id"],
        "rootThreadId": root["id"],
        "turnId": turn["id"],
        "chatTitle": "Attention root",
        "category": "bash",
        "explicit": False,
        "actionable": True,
        "connected": True,
        "remoteSessionId": None,
        "scheduled": False,
    }
    assert {k: row.get(k) for k in expected} == expected, row
    assert secret not in json.dumps(row) and "printf" not in json.dumps(row), row
    ready = next(f for f in events.frames if f.get("type") == "ready")
    assert "attention" in ready["entities"], ready
    until(lambda: events.attention(hid), "no attention event named the computer")
    deny(lambda: cloud.dismiss_attention(row["id"]), 409)
    if pushes:
        alert = alerted(request["permissionId"], "an approval")
        assert alert["aps"]["alert"]["loc-key"] == "push.approval.bash", alert
        assert alert["aps"]["category"] == "haarness.approval", alert
        assert alert["haarness"]["attentionId"] == row["id"], alert
        assert alert["haarness"]["childThreadId"] is None, alert
        assert secret not in json.dumps(alert) and "printf" not in json.dumps(alert)
    reply(runtime, request)
    until(
        lambda: not attention(itemId=request["permissionId"]),
        "an answered approval stayed open",
    )
    completed(turn)
    done = until(
        lambda: finished_rows(turn["id"]), "the finished chat was not reported"
    )
    expected = {
        "kind": "turn_completed",
        "hostId": hid,
        "projectId": pid,
        "threadId": root["id"],
        "rootThreadId": root["id"],
        "chatTitle": "Attention root",
        "scheduled": False,
    }
    assert {k: done[0].get(k) for k in expected} == expected, done
    if pushes:
        withdrawn([row["id"]], "an answered approval")
        alert = alerted(turn["id"], "a finished chat")
        assert alert["aps"]["alert"]["loc-key"] == "push.done", alert
    proof(
        "an approval reaches Cloud with its category and chat title but no command, and the answer resolves it"
    )

    # A helper agent's approval belongs to the chat that started it, and only
    # that chat's turns are reported as finished, never the helper's.
    parent = chat(runtime, "Helper parent")
    task = {
        "prompt": "!tool bash "
        + json.dumps({"command": "printf helper > helper-proof.txt"}),
        "agent": "general",
        "description": "Helper proof",
    }
    parent_turn = runtime.start_turn(
        parent["id"],
        {"requestId": str(uuid.uuid4()), "text": "!tool task " + json.dumps(task)},
    )

    def helper_request():
        for waiting in runtime.list_permissions():
            if waiting["threadId"] == parent["id"]:
                reply(runtime, waiting)  # the delegation itself, if it asks
            elif waiting.get("rootThreadId") == parent["id"]:
                return waiting

    request = until(helper_request, "the helper agent did not ask")
    helper = request["threadId"]
    row = until(
        lambda: one(itemId=request["permissionId"]),
        "the helper's approval did not reach the attention list",
    )
    expected = {
        "threadId": helper,
        "rootThreadId": parent["id"],
        "chatTitle": "Helper parent",
        "category": "bash",
    }
    assert {k: row.get(k) for k in expected} == expected, row
    if pushes:
        alert = alerted(request["permissionId"], "a helper agent's approval")
        assert alert["haarness"]["threadId"] == parent["id"], alert
        assert alert["haarness"]["childThreadId"] == helper, alert
    reply(runtime, request)
    completed(parent_turn)
    until(
        lambda: finished_rows(parent_turn["id"]), "the parent chat's finish was missing"
    )
    followup = runtime.start_turn(
        helper, {"requestId": str(uuid.uuid4()), "text": "HELPER_FOLLOWUP_OK"}
    )
    completed(followup)
    time.sleep(4)  # a helper's finish would have settled and been sent by now
    finished = [
        r for r in attention(rootThreadId=parent["id"]) if r["kind"].startswith("turn_")
    ]
    assert [r["turnId"] for r in finished] == [parent_turn["id"]], finished
    assert not finished_rows(request["turnId"]) and not finished_rows(followup["id"])
    proof(
        "a helper agent's approval opens its parent chat, and only the parent's turn is reported finished, once"
    )

    # A second project: its server is watched while it runs, closing it for
    # remote access withdraws what waits there, and a stopped server is never
    # started again by watching.
    other = project_folder("second")
    opened = host.host_open({"path": str(other)})
    pid2 = opened["id"]
    second = host.workspace(pid2)
    asking = chat(second, "Second project chat")
    question_turn = second.start_turn(
        asking["id"],
        {
            "requestId": str(uuid.uuid4()),
            "text": "!tool question "
            + json.dumps(
                {
                    "prompt": "Which colour? " + secret,
                    "choices": ["Red", "Blue"],
                    "custom": False,
                }
            ),
        },
    )
    question = pending(second, asking["id"], questions=True)
    row = until(
        lambda: one(itemId=question["questionId"]),
        "the second project's question did not reach the attention list",
    )
    expected = {
        "kind": "question",
        "hostId": hid,
        "projectId": pid2,
        "projectName": opened["name"],
        "threadId": asking["id"],
        "rootThreadId": asking["id"],
        "chatTitle": "Second project chat",
        "category": None,
        "custom": False,
        "explicit": False,
        "actionable": True,
    }
    assert {k: row.get(k) for k in expected} == expected, row
    assert secret not in json.dumps(row), row
    if pushes:
        alert = until(
            lambda: pushes.alerts(question["questionId"]),
            "no notification for a question",
            PUSH_WAIT,
        )[0]
        assert alert["aps"]["alert"]["loc-key"] == "push.question", alert
        assert alert["aps"]["category"] == "haarness.view", alert  # choices only
        assert secret not in json.dumps(alert), alert
    host.host_close(pid2)
    until(
        lambda: not attention(itemId=question["questionId"]),
        "a closed project's question stayed in the attention list",
    )
    assert host.host_open({"path": str(other)})["id"] == pid2
    again = until(
        lambda: one(itemId=question["questionId"]),
        "a reopened project's question did not come back",
    )
    assert again["id"] == row["id"], (again, row)
    second.reply_question(question["questionId"], {"answer": "Blue"})
    until(
        lambda: not attention(itemId=question["questionId"]),
        "an answered question stayed open",
    )
    completed(question_turn, second)
    discovery = ROOT / "runtime/cache/workspaces" / pid2 / "runtime.json"
    assert discovery.exists(), discovery
    second.shutdown_server()
    until(lambda: not discovery.exists(), "the second project's server did not stop")
    time.sleep(12)  # more than two of the watcher's scans
    assert not discovery.exists(), "watching started a stopped project's server"
    commands = subprocess.run(
        ["ps", "-axww", "-o", "command="], capture_output=True, text=True, check=True
    ).stdout
    assert str(other) not in commands, "a server for the stopped project runs"
    assert any(w["id"] == pid2 for w in host.host_projects()["workspaces"])
    host.host_close(pid2)
    proof(
        "a second project's question is reported while its server runs, withdrawn while the project is closed, and its stopped server stays stopped"
    )

    # A project server started by another Haarness build (Desktop's own, say)
    # is watched too, but Cloud cannot route an answer to it while its turn
    # runs, so its request is marked as one to answer on the computer.
    other_build = ROOT / "other-build" / "haarness"
    other_build.parent.mkdir()
    if subprocess.run(["cp", "-c", BIN, str(other_build)]).returncode != 0:
        shutil.copy2(BIN, other_build)  # no copy-on-write clone here
    elsewhere = project_folder("other-build-project")
    started = json.loads(
        subprocess.run(
            [str(other_build), "--workspace", str(elsewhere), "desktop"],
            env=env,
            input='{"action":"connect"}',
            text=True,
            capture_output=True,
            check=True,
            timeout=120,
        ).stdout
    )
    foreign = Haarness(started["url"], token=started["token"])
    pid3 = until(
        lambda: next(
            (
                w["id"]
                for w in host.host_projects()["workspaces"]
                if w["path"] == str(elsewhere)
            ),
            None,
        ),
        "the other build's project is not in the catalog",
    )
    foreign_chat = chat(foreign, "Started by another build")
    foreign_turn = ask(foreign, foreign_chat["id"], "printf other > other-proof.txt")
    request = pending(foreign, foreign_chat["id"])
    row = until(
        lambda: one(itemId=request["permissionId"]),
        "the other build's approval was not reported",
    )
    expected = {
        "projectId": pid3,
        "chatTitle": "Started by another build",
        "category": "bash",
        "actionable": False,
        "connected": True,
    }
    assert {k: row.get(k) for k in expected} == expected, row
    if pushes:
        alert = alerted(
            request["permissionId"], "an approval only the computer can answer"
        )
        assert alert["aps"]["category"] == "haarness.view", alert  # no answer buttons
    try:
        host.workspace(pid3).reply_permission(
            request["permissionId"], {"decision": "once"}
        )
    except ApiError as error:
        assert error.status >= 400, error
    else:
        raise AssertionError("Cloud answered a server it was told it cannot reach")
    assert any(
        p["permissionId"] == request["permissionId"] for p in foreign.list_permissions()
    )
    reply(foreign, request)
    completed(foreign_turn, foreign)
    until(
        lambda: not attention(itemId=request["permissionId"]),
        "an answer given on the computer was not reported",
    )
    foreign.shutdown_server()
    foreign.close()
    other_build.unlink()
    host.host_close(pid3)
    proof(
        "a project server from another Haarness build is reported, marked to answer on the computer"
    )

    # A network drop: the connector keeps running and reconnects by itself.
    # What still waits stays one open row throughout, shown offline meanwhile;
    # an answer given on the computer while offline resolves its row, and a
    # turn that finished offline is reported once after reconnecting.
    held = chat(runtime, "Waits through a drop")
    held_turn = ask(runtime, held["id"], "printf held > held-proof.txt")
    answered = chat(runtime, "Answered while offline")
    answered_turn = ask(runtime, answered["id"], "printf answered > answered-proof.txt")
    waiting = pending(runtime, held["id"])
    offline = pending(runtime, answered["id"])
    row = until(
        lambda: one(itemId=waiting["permissionId"]), "the approval was not reported"
    )
    until(lambda: one(itemId=offline["permissionId"]), "the approval was not reported")
    if pushes:
        alerted(waiting["permissionId"], "an approval that waits through a drop")
    watch = Presence(waiting["permissionId"])
    watch.start()
    proxy.cut()
    until(
        lambda: not cloud.get_remote_host(hid)["connected"],
        "Cloud did not see the computer go offline",
    )
    until(
        lambda: (one(itemId=waiting["permissionId"]) or {}).get("connected") is False,
        "an offline computer's request still shows as connected",
    )
    local.reply_permission(offline["permissionId"], {"decision": "once"})
    completed(answered_turn)
    time.sleep(3)
    assert one(itemId=offline["permissionId"]), "Cloud cannot know of an answer yet"
    proxy.resume()
    until(
        lambda: (one(itemId=waiting["permissionId"]) or {}).get("connected"),
        "the request did not come back online",
        60,
    )
    until(
        lambda: not attention(itemId=offline["permissionId"]),
        "an answer given while offline was not reported",
    )
    until(
        lambda: finished_rows(answered_turn["id"]),
        "a turn that finished while offline was not reported",
    )
    time.sleep(3)
    watch.stop()
    assert not watch.gaps, ("a request left the list across the drop", watch.gaps)
    assert False in watch.samples and watch.samples[-1] is True, watch.samples
    assert one(itemId=waiting["permissionId"])["id"] == row["id"]
    assert len(finished_rows(answered_turn["id"])) == 1
    if pushes:
        assert not pushes.evicts([row["id"]]), "a request that still waits was cleared"
        alerted(answered_turn["id"], "a turn that finished offline")
    proof(
        "a network drop shows requests offline, keeps each one open row, and reconnecting reports what changed meanwhile, once"
    )

    # The connector itself restarts: the same host, the same open row, no gap.
    watch = Presence(waiting["permissionId"])
    watch.start()
    seen = len(events.attention(hid))
    stop(process)
    until(
        lambda: (one(itemId=waiting["permissionId"]) or {}).get("connected") is False,
        "a stopped connector's request still shows as connected",
    )
    process, again = connector()
    assert again["id"] == hid
    until(
        lambda: (one(itemId=waiting["permissionId"]) or {}).get("connected"),
        "the request did not come back with the connector",
    )
    time.sleep(3)
    watch.stop()
    assert not watch.gaps, ("a request left the list across a restart", watch.gaps)
    assert one(itemId=waiting["permissionId"])["id"] == row["id"]
    # Going offline and coming back online are one event each; a first
    # snapshot that resolved and reopened the request would add two more.
    changes = events.attention(hid)[seen:]
    assert len(changes) == 2, ("attention events across a restart", changes)
    if pushes:
        assert not pushes.evicts([row["id"]]), "a request that still waits was cleared"
    reply(runtime, waiting)
    completed(held_turn)
    until(lambda: not attention(itemId=waiting["permissionId"]), "the answer was lost")
    until(lambda: finished_rows(held_turn["id"]), "the finish after a restart was lost")
    proof(
        "a restarted connector keeps the same host and open rows, without a gap or a repeat"
    )

    # The computer shares a chat through /remote-control while its host-mode
    # connector runs: both report the chat's approval, the phone hears once.
    twins = chat(runtime, "Shared twice")
    local.manage_resource(
        "remote-control", {"action": "start", "threadId": twins["id"]}
    )
    share = until(
        lambda: next(
            (
                s
                for s in cloud.list_remote_sessions()["items"]
                if s["threadId"] == twins["id"] and s["connected"]
            ),
            None,
        ),
        "the /remote-control share did not connect",
    )
    hosts.append(share["hostId"])
    twin_turn = ask(runtime, twins["id"], "printf twins > twins-proof.txt")
    request = pending(runtime, twins["id"])
    host_row = until(
        lambda: one(hostId=hid, itemId=request["permissionId"]),
        "the host-mode connector did not report the approval",
    )
    chat_row = until(
        lambda: one(hostId=share["hostId"], itemId=request["permissionId"]),
        "the /remote-control connector did not report the approval",
    )
    assert chat_row["remoteSessionId"] == share["id"], chat_row
    assert host_row.get("remoteSessionId") is None, host_row
    if pushes:
        alert = alerted(request["permissionId"], "an approval two connectors report")
        assert alert["aps"]["badge"] == 1, alert
        assert alert["haarness"]["attentionId"] in (host_row["id"], chat_row["id"])
    reply(runtime, request)
    until(
        lambda: not attention(itemId=request["permissionId"]),
        "an answered approval stayed open",
    )
    completed(twin_turn)
    until(
        lambda: len(finished_rows(twin_turn["id"])) == 2,
        "both connectors report the finish",
    )
    if pushes:
        withdrawn([host_row["id"], chat_row["id"]], "an approval two connectors report")
        alerted(twin_turn["id"], "a finish two connectors report")
    local.manage_resource("remote-control", {"action": "stop", "threadId": twins["id"]})
    until(
        lambda: (
            not any(
                s["threadId"] == twins["id"]
                for s in cloud.list_remote_sessions()["items"]
            )
        ),
        "stopping the share left its publication",
    )
    proof(
        "a chat shared by /remote-control beside the host-mode connector is reported by both and announced once"
    )
    if pushes:
        # One more request, announced and withdrawn, proves the dispatcher ran
        # again after everything above: each was announced exactly once.
        last = chat(runtime, "Last notification")
        last_turn = ask(runtime, last["id"], "printf last > last-proof.txt")
        request = pending(runtime, last["id"])
        row = until(lambda: one(itemId=request["permissionId"]), "not reported")
        alerted(request["permissionId"], "the last approval")
        reply(runtime, request)
        withdrawn([row["id"]], "the last approval")
        completed(last_turn)
        repeated = {
            label: len(pushes.alerts(item))
            for item, label in announced.items()
            if len(pushes.alerts(item)) != 1
        }
        assert not repeated, ("announced more than once", repeated)
        proof(f"{len(announced)} notifications, each sent exactly once")
    if phone:  # the last notification check: a short-lived sign-in is enough
        phone.unregister_push_device()
        phone.close()
        phone = None
    a = runtime.create_thread(
        {"title": "Shared root", "provider": "echo", "model": "echo-1"}
    )
    b = runtime.create_thread(
        {"title": "Private sibling", "provider": "echo", "model": "echo-1"}
    )
    child = runtime.create_thread(
        {"title": "Child", "parentId": a["id"], "provider": "echo", "model": "echo-1"}
    )
    shared = cloud.publish_remote_session(
        hid, {"projectId": pid, "threadId": a["id"], "title": "Published proof"}
    )
    session = cloud.remote_session(shared["id"])
    assert {t["id"] for t in session.list_threads()} == {a["id"], child["id"]}
    deny(lambda: session.get_thread(b["id"]))
    deny(lambda: session.create_thread({}))
    deny(lambda: session.list_ptys())
    deny(lambda: session.shutdown_server())
    result = session.chat(a["id"], "REMOTE_SHARED_CHAT_OK")
    assert result["turn"]["status"] == "completed"
    result = session.chat(child["id"], "REMOTE_CHILD_CHAT_OK")
    assert result["turn"]["status"] == "completed"
    proof(
        "published conversation tree, child control and sibling/terminal/shutdown denial"
    )
    empty = hashlib.sha256(b"").hexdigest()
    binary = bytes(range(256)) * (10 * 1024 * 1024 // 256)
    uploaded = runtime.write_file(
        {
            "path": "large-upload.bin",
            "content": base64.b64encode(binary).decode(),
            "encoding": "base64",
            "revision": empty,
        },
        options={"timeout": 90},
    )
    downloaded = session.read_file(
        {"path": "large-upload.bin"}, options={"timeout": 90}
    )
    assert (
        downloaded["size"] == len(binary)
        and base64.b64decode(downloaded["content"]) == binary
    )
    proof("10 MiB binary upload/read across bounded relay chunks")

    def concurrent(n):
        return runtime.get_thread(a["id"] if n % 2 else b["id"])["id"]

    with ThreadPoolExecutor(max_workers=20) as pool:
        actual = list(pool.map(concurrent, range(200)))
    assert actual == [a["id"] if n % 2 else b["id"] for n in range(200)]
    proof("200 concurrent SDK reads without crossed responses")
    turn = runtime.start_turn(
        a["id"],
        {
            "requestId": str(uuid.uuid4()),
            "text": "!tool bash "
            + json.dumps(
                {"command": "sleep 3; printf 'accepted-once\\n' >> accepted-count.txt"}
            ),
        },
    )
    permission = until(
        lambda: next(
            (p for p in runtime.list_permissions() if p["threadId"] == a["id"]), None
        ),
        "approval missing",
    )
    runtime.reply_permission(permission["permissionId"], {"decision": "once"})
    stop(process)
    until(
        lambda: not cloud.get_remote_host(hid)["connected"],
        "host did not become offline",
    )
    deny(lambda: runtime.info(), 409)
    assert local.wait_turn(turn["id"], {"timeout_ms": 20000})["status"] == "completed"
    process, again = connector()
    assert again["id"] == hid
    assert runtime.get_turn(turn["id"])["status"] == "completed"
    assert (
        runtime.read_file({"path": "accepted-count.txt"})["content"]
        == "accepted-once\n"
    )
    proof("approval, disconnect during work, stable reconnect and no mutation replay")
    # Many held streams never crowd out a request, and abandoned readers are released.
    from contextlib import ExitStack

    with httpx.Client(timeout=10) as raw, ExitStack() as stack:
        for n in range(48):
            response = stack.enter_context(
                raw.stream(
                    "GET",
                    runtime.base_url + "/v1/events",
                    headers={"Authorization": "Bearer " + env["HAARNESS_API_KEY"]},
                )
            )
            assert response.status_code == 200, (n, response.status_code)
        assert (
            raw.get(
                runtime.base_url + "/v1/info",
                headers={"Authorization": "Bearer " + env["HAARNESS_API_KEY"]},
            ).status_code
            == 200
        )
    until(lambda: runtime.info(), "abandoned readers were not released")
    proof("48 held streams beside a request, and abandoned-reader cleanup")
    # Close advertisement without deleting history; re-opening retains the same identity.
    host.host_close(pid)
    assert not host.host_projects()["workspaces"]
    deny(lambda: runtime.info(), 400)
    assert host.host_open({"path": str(WORK)})["id"] == pid
    assert runtime.get_thread(a["id"])["id"] == a["id"]
    proof("close/reopen project retains local files and conversations")
    local.shutdown_server()

    def recovered_runtime():
        try:
            return runtime.get_thread(a["id"])["id"] == a["id"]
        except (ApiError, httpx.HTTPError):
            return False

    until(recovered_runtime, "native daemon restart did not recover", 60)
    endpoint = json.loads(
        subprocess.run(
            args + ["desktop"],
            env=env,
            input='{"action":"connect"}',
            text=True,
            capture_output=True,
            check=True,
            timeout=120,
        ).stdout
    )
    local.close()
    local = Haarness(endpoint["url"], token=endpoint["token"])
    assert (
        runtime.read_file({"path": ".hidden-proof"})["content"] == "remote-hidden-proof"
    )
    proof(
        "local daemon replacement automatically refreshes connector address and credentials"
    )
    terminal = runtime.create_pty({"title": "SDK remote proof", "cols": 80, "rows": 24})
    with runtime.connect_pty(terminal["id"]) as socket:
        socket.send("printf 'SDK_REMOTE_PTY_OK\\n'\n")
        output = b""
        while b"\r\nSDK_REMOTE_PTY_OK\r\n" not in output:
            chunk = socket.recv(timeout=10)
            if isinstance(chunk, bytes):
                output += chunk
        time.sleep(
            42
        )  # longer than relay's idle-frame limit: native heartbeat must preserve PTY
        socket.send("printf 'SDK_REMOTE_IDLE_OK\\n'\n")
        while b"\r\nSDK_REMOTE_IDLE_OK\r\n" not in output:
            chunk = socket.recv(timeout=10)
            if isinstance(chunk, bytes):
                output += chunk
    runtime.delete_pty(terminal["id"])
    proof("PTY binary/control frames and idle keepalive beyond 40 seconds")

    async def asynchronous():
        async with AsyncHaarnessCloud(BASE, token=env["HAARNESS_API_KEY"]) as c:
            rt = c.remote_host(hid).workspace(pid)
            try:
                turns = await asyncio.gather(
                    *(rt.get_thread(a["id"]) for _ in range(16))
                )
                assert all(t["id"] == a["id"] for t in turns)
                assert (await rt.chat(b["id"], "ASYNC_REMOTE_OK"))["turn"][
                    "status"
                ] == "completed"
            finally:
                await rt.close()

    asyncio.run(asynchronous())
    proof("async Python project and chat adapter")
    test_env = dict(
        env,
        HAARNESS_CLOUD_API_URL=BASE,
        HAARNESS_REMOTE_HOST_ID=hid,
        HAARNESS_REMOTE_PROJECT_ID=pid,
        HAARNESS_REMOTE_THREAD_ID=b["id"],
    )
    subprocess.run(
        ["node", str(REPO / "sdks/tests/remote_e2e.mjs")],
        env=test_env,
        check=True,
        timeout=60,
    )
    proof("TypeScript host, project, chat, files, SSE and PTY adapters")
    stop(process)
    process, selected = connector(a["id"])
    deny(lambda: cloud.remote_host(selected["id"]).host_projects())
    pub = next(
        s
        for s in cloud.list_remote_sessions()["items"]
        if s["hostId"] == selected["id"]
    )
    scoped = cloud.remote_session(pub["id"])
    assert {t["id"] for t in scoped.list_threads()} == {a["id"], child["id"]}
    forged = cloud.publish_remote_session(
        selected["id"],
        {"projectId": pid, "threadId": b["id"], "title": "Must remain inaccessible"},
    )
    deny(lambda: cloud.remote_session(forged["id"]).list_threads(), 409)
    proof("session-only connector rejects host access and forged sibling publication")
    # A chat-only connector reports its shared tree only, each row linked to
    # the publication, even beside a forged publication of the sibling.
    tree_turn = ask(local, child["id"], "printf tree > tree-proof.txt")
    sibling_turn = ask(local, b["id"], "printf sibling > sibling-proof.txt")
    tree = pending(local, child["id"])
    sibling = pending(local, b["id"])
    row = until(
        lambda: one(hostId=selected["id"], itemId=tree["permissionId"]),
        "the shared tree's approval was not reported",
    )
    expected = {
        "remoteSessionId": pub["id"],
        "projectId": pid,
        "threadId": child["id"],
        "rootThreadId": a["id"],
        "chatTitle": local.get_thread(a["id"])["title"],
        "kind": "permission",
        "category": "bash",
        "connected": True,
    }
    assert {k: row.get(k) for k in expected} == expected, row
    time.sleep(3)
    assert not attention(hostId=selected["id"], threadId=b["id"]), (
        "a chat-only connector reported a sibling"
    )
    assert not attention(itemId=sibling["permissionId"])
    reply(local, tree)
    reply(local, sibling)
    completed(tree_turn)
    completed(sibling_turn)
    until(
        lambda: not attention(itemId=tree["permissionId"]),
        "an answered approval stayed open",
    )
    shared_turn = local.start_turn(
        a["id"], {"requestId": str(uuid.uuid4()), "text": "SHARED_ROOT_DONE"}
    )
    completed(shared_turn)
    done = until(
        lambda: finished_rows(shared_turn["id"]), "the shared chat's finish was lost"
    )
    assert (
        done[0]["remoteSessionId"] == pub["id"] and done[0]["hostId"] == selected["id"]
    )
    time.sleep(3)
    assert not finished_rows(tree_turn["id"]) and not finished_rows(sibling_turn["id"])
    proof(
        "a chat-only connector reports only its shared tree, linked to the publication"
    )
    cli("remote", "disable", selected["id"])
    process.wait(15)
    assert not next(
        v
        for v in json.loads(cli("remote", "status", "--json"))["items"]
        if v["id"] == selected["id"]
    )["running"]
    cli(
        "remote",
        "enable",
        "--name",
        "Remote stress " + ROOT.name + " shared",
        "--chat",
        a["id"],
    )
    until(
        lambda: cloud.get_remote_host(selected["id"])["connected"],
        "background enable failed",
    )
    cli("remote", "disable", selected["id"])
    cli("remote", "forget", selected["id"])
    deny(lambda: cloud.get_remote_host(selected["id"]), 404)
    process, replacement = connector(a["id"])
    assert replacement["id"] != selected["id"]
    assert local.get_thread(a["id"])["id"] == a["id"]
    stop(process)
    proof(
        "CLI enable/status/disable/forget and explicit fresh registration retain local chats"
    )
finally:
    if proxy.listener is None:
        proxy.resume()  # the CLI below reaches Cloud through it
    for p in procs:
        if p.poll() is None:
            stop(p)
    for hostid in hosts:
        try:
            cli("remote", "disable", hostid)
        except Exception:
            pass
        try:
            cloud.revoke_remote_host(hostid)
        except ApiError as error:
            if error.status != 404:
                raise
    if local:
        try:
            local.shutdown_server()
        except Exception:
            pass
        local.close()
    if phone:
        try:
            phone.unregister_push_device()
        except Exception as error:
            print("Push device left registered:", error, flush=True)
        phone.close()
    events.client.close()
    cloud.close()
    proxy.cut()
    (ROOT / "checks.json").write_text(json.dumps(checks, indent=2))
    print("Evidence:", ROOT, flush=True)
