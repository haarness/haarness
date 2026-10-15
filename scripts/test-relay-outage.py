#!/usr/bin/env python3
"""Cloud outage scenario for the /remote-control connector, against a fake Cloud.

The shared local Cloud stacks belong to other sessions, so this plays Cloud
itself: the sign-in (`/api/v1/me`, `/oauth/token`), host registration and
publication, the session status and the relay WebSocket, which routes one
request into the shared chat so each connection proves itself at once.

  scripts/test-relay-outage.py [state-folder]   (HAARNESS_BIN picks the binary, default target/debug/haarness)

1. Start a project server (echo provider) and a chat, share it with
   `remote run --chat`, and wait until Cloud routed a request through it.
2. Expire the stored access token, kill the fake Cloud (as if cloud-api
   stopped) for 60 s, and start it again on the same port.
3. The connector must still run, renew the sign-in once Cloud is back,
   reconnect, and have said one "Could not reach" and one "reconnected" line.
"""

import datetime
import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

REPO = pathlib.Path(__file__).resolve().parents[1]


def serve(port: int, chat: str, events: str, project: str) -> None:
    """The fake Cloud: one process, killed for the outage."""
    import asyncio

    from aiohttp import WSMsgType, web

    state = {"renewals": 0, "connected": 0, "project": project}

    def log(event: str, **fields) -> None:
        with open(events, "a", encoding="utf-8") as file:
            file.write(json.dumps({"event": event, "at": time.time(), **fields}) + "\n")

    account = "00000000-0000-4000-8000-00000000a001"

    async def me(request):
        return web.json_response(
            {
                "user": {"id": "u1", "displayName": "Outage test", "email": "outage@fixture.invalid"},
                "accounts": [{"id": account, "name": "Outage test", "role": "owner"}],
                "defaultAccountId": account,
            }
        )

    async def token(request):
        form = await request.post()
        state["renewals"] += 1
        log("renewal", grant=form.get("grant_type"))
        return web.json_response(
            {
                "access_token": f"renewed-{state['renewals']}",
                "refresh_token": f"refresh-{state['renewals']}",
                "expires_in": 600,
            }
        )

    async def host(request):
        return web.json_response({"id": "host"})

    async def publish(request):
        body = await request.json()
        state["project"] = body.get("projectId")
        log("published", title=body.get("title"), project=state["project"])
        return web.json_response({"id": "00000000-0000-4000-8000-0000000000b1", "title": body.get("title")})

    async def session(request):
        return web.json_response({"id": request.match_info["id"], "connected": state["connected"] > 0})

    async def relay(request):
        bearer = request.headers.get("authorization", "")
        ws = web.WebSocketResponse(autoping=True)
        await ws.prepare(request)
        state["connected"] += 1
        log("relay_open", bearer=bearer.removeprefix("Bearer "))
        await ws.send_str(json.dumps({"type": "ready", "version": 1}))
        request_id = str(uuid.uuid4())
        await ws.send_str(
            json.dumps(
                {
                    "type": "open",
                    "id": request_id,
                    "method": "GET",
                    "path": f"/v1/host/workspaces/{state['project']}/v1/threads/{chat}",
                    "headers": [],
                    "body": "",
                    "websocket": False,
                    "thread": chat,
                }
            )
        )
        async for message in ws:
            if message.type == WSMsgType.TEXT:
                frame = json.loads(message.data)
                if frame.get("type") == "end" and frame.get("id") == request_id:
                    log("served")
        state["connected"] -= 1
        log("relay_closed")
        return ws

    app = web.Application()
    app.router.add_get("/api/v1/me", me)
    app.router.add_post("/oauth/token", token)
    app.router.add_post("/api/v1/remote-hosts", host)
    app.router.add_post("/api/v1/remote-hosts/{id}/sessions", publish)
    app.router.add_get("/api/v1/remote-sessions/{id}", session)
    app.router.add_get("/api/v1/remote-hosts/{id}/connect", relay)
    log("cloud_started", port=port)
    web.run_app(app, host="127.0.0.1", port=port, reuse_address=True, print=None)


def events(path: pathlib.Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def wait_for(check, timeout: float, what: str):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.5)
    raise SystemExit(f"FAIL: timed out waiting for {what}")


def credentials(path: pathlib.Path, api: str, expires: datetime.datetime, access: str, refresh: str) -> None:
    path.write_text(
        json.dumps(
            {
                "apiUrl": api,
                "accessToken": access,
                "accessExpiresAt": expires.isoformat().replace("+00:00", "Z"),
                "refreshToken": refresh,
            }
        )
    )
    os.chmod(path, 0o600)


def main() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == "serve":
        serve(int(sys.argv[2]), sys.argv[3], sys.argv[4], sys.argv[5])
        return
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else tempfile.mkdtemp(prefix="haarness-relay-outage-")).resolve()
    binary = str(pathlib.Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve())
    shutil.rmtree(root, ignore_errors=True)
    workspace, data, account, home = (root / name for name in ("workspace", "data", "account", "home"))
    for folder in (workspace, data, account, home):
        folder.mkdir(parents=True)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    api = f"http://127.0.0.1:{port}/"
    event_log = root / "cloud-events.jsonl"
    connector_log = root / "connector.log"
    env = {
        **os.environ,
        "HAARNESS_HOME": str(home),
        "HAARNESS_CLOUD_API_URL": api.rstrip("/"),
        "HAARNESS_CLOUD_WEB_ORIGIN": api.rstrip("/"),
        "HAARNESS_CLOUD_CONFIG_DIR": str(account),
        "HAARNESS_TEST_PROVIDER": "1",
        "RUST_LOG": "warn",
    }
    env.pop("HAARNESS_API_KEY", None)
    base = [binary, "--data-dir", str(data), "--workspace", str(workspace)]
    credentials(account / "credentials.json", api, datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(minutes=10), "first", "refresh-0")

    connect = subprocess.run(base + ["desktop"], input=json.dumps({"action": "connect"}), capture_output=True, text=True, env=env, timeout=60)
    if connect.returncode != 0:
        raise SystemExit(f"FAIL: project server did not start: {connect.stderr}")
    server = json.loads(connect.stdout)
    request = urllib.request.Request(
        server["url"] + "/v1/threads",
        data=b"{}",
        headers={"authorization": f"Bearer {server['token']}", "content-type": "application/json"},
        method="POST",
    )
    chat = json.loads(urllib.request.urlopen(request, timeout=10).read())["id"]

    def start_cloud():
        # A restarted Cloud still knows the published project.
        project = next((e["project"] for e in events(event_log) if e["event"] == "published"), "")
        return subprocess.Popen([sys.executable, __file__, "serve", str(port), chat, str(event_log), project], stdout=subprocess.DEVNULL, stderr=open(root / "cloud.log", "a"))

    cloud = start_cloud()
    wait_for(lambda: any(e["event"] == "cloud_started" for e in events(event_log)), 20, "the fake Cloud")
    connector = subprocess.Popen(
        base + ["remote", "run", "--chat", chat, "--name", "outage-e2e"],
        stdout=open(connector_log, "a"),
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    result = {"port": port, "chat": chat}
    try:
        wait_for(lambda: any(e["event"] == "served" for e in events(event_log)), 90, "the first relayed request")
        before = connector_log.read_text()
        assert "Remote host connected" in before, before
        # The sign-in expires while Cloud is away.
        credentials(account / "credentials.json", api, datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(minutes=1), "first", "refresh-0")
        marker = len(before)
        outage = time.time()
        cloud.kill()
        cloud.wait()
        time.sleep(60)
        assert connector.poll() is None, "the connector stopped during the outage:\n" + connector_log.read_text()
        cloud = start_cloud()
        restarted = time.time()
        wait_for(lambda: any(e["event"] == "served" and e["at"] > restarted for e in events(event_log)), 120, "a relayed request after the outage")
        wait_for(lambda: "Remote host reconnected after" in connector_log.read_text()[marker:], 20, "the reconnected line")
        time.sleep(1)
        during = connector_log.read_text()[marker:]
        lines = [line for line in during.splitlines() if line.strip()]
        unreachable = [line for line in lines if line.startswith("Could not reach Haarness Cloud")]
        reconnected = [line for line in lines if line.startswith("Remote host reconnected after")]
        after = [e for e in events(event_log) if e["at"] > restarted]
        renewals = [e for e in after if e["event"] == "renewal"]
        bearers = [e["bearer"] for e in after if e["event"] == "relay_open"]
        status = json.loads(
            urllib.request.urlopen(f"{api}api/v1/remote-sessions/00000000-0000-4000-8000-0000000000b1", timeout=5).read()
        )
        result.update(
            outage_seconds=round(restarted - outage),
            lines=lines,
            renewals=len(renewals),
            bearers_after=bearers,
            connected=status["connected"],
            connector_running=connector.poll() is None,
        )
        assert len(unreachable) == 1, lines
        assert len(reconnected) == 1, lines
        assert renewals and bearers and bearers[0].startswith("renewed-"), (renewals, bearers)
        assert status["connected"] is True
        assert connector.poll() is None
        result["verdict"] = "PASS"
    finally:
        print(json.dumps(result, indent=2))
        if connector.poll() is None:
            os.killpg(connector.pid, signal.SIGTERM)
            try:
                connector.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(connector.pid, signal.SIGKILL)
        cloud.kill()
        subprocess.run(base + ["serve", "--stop"], env=env, capture_output=True, timeout=60)


if __name__ == "__main__":
    main()
