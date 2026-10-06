#!/usr/bin/env python3
"""Opt-in real native connector + Cloud + Python/TS SDK lifecycle/stress test.

HAARNESS_REMOTE_E2E=1 HAARNESS_CLOUD_API_URL=http://127.0.0.1:19080
HAARNESS_API_KEY must allow remote:read, remote:connect, remote:control.
Uses the built CLI, isolated local files/state, echo provider, and owned registrations.
The browser/model proof is a separate pass; this test never prints credentials.
"""

import asyncio, base64, hashlib, json, os, subprocess, time, uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
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
WORK = ROOT / "workspace"
(WORK / ".haarness").mkdir(parents=True)
(WORK / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\nwrite="ask"\n[lsp]\nenabled=false\n[lazy_tools]\nmode="off"\n'
)
(WORK / ".hidden-proof").write_text("remote-hidden-proof")
subprocess.run(["git", "init", "-q", str(WORK)], check=True)
env = {
    k: os.environ[k]
    for k in [
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "TMPDIR",
        "HAARNESS_API_KEY",
        "HAARNESS_CLOUD_API_URL",
    ]
    if k in os.environ
}
env.update(
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
    cli("remote", "disable", selected["id"])
    process.wait(15)
    assert not next(
        v
        for v in json.loads(cli("remote", "status"))["items"]
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
    cloud.close()
    (ROOT / "checks.json").write_text(json.dumps(checks, indent=2))
    print("Evidence:", ROOT, flush=True)
