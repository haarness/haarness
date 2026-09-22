#!/usr/bin/env python3
"""Black-box PTY REST/WebSocket lifecycle proof for the tmux gate."""

import asyncio
import json
import os
import sys
import time
import urllib.error
import urllib.request

import websockets


BASE, TOKEN = sys.argv[1:3]


def request(method, path, body=None, authenticated=True):
    headers = {}
    if authenticated:
        headers["Authorization"] = f"Bearer {TOKEN}"
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=5) as response:
        payload = response.read()
        return json.loads(payload) if payload else None


async def receive_until(socket, marker):
    output = bytearray()
    cursor = 0
    deadline = asyncio.get_running_loop().time() + 5
    while marker not in output:
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            raise AssertionError(f"timed out waiting for {marker!r}; got {output!r}")
        message = await asyncio.wait_for(socket.recv(), remaining)
        if isinstance(message, str):
            control = json.loads(message)
            if control.get("type") == "cursor":
                cursor = control["end"]
        else:
            output.extend(message)
    # Consume the cursor acknowledgement that follows the matching binary
    # frame, otherwise reconnecting could deliberately request stale output.
    while True:
        try:
            message = await asyncio.wait_for(socket.recv(), 0.2)
        except asyncio.TimeoutError:
            break
        if isinstance(message, str):
            control = json.loads(message)
            if control.get("type") == "cursor":
                cursor = control["end"]
                break
        else:
            output.extend(message)
    return bytes(output), cursor


async def main():
    try:
        request("GET", "/v1/ptys", authenticated=False)
    except urllib.error.HTTPError as error:
        assert error.code == 401, error
    else:
        raise AssertionError("PTY routes accepted an unauthenticated request")

    # Any terminal left by the real TUI portion of the gate must still be
    # alive after that client pane was killed: client lifetime is not process
    # lifetime. Clear it before the deterministic protocol test.
    inherited = request("GET", "/v1/ptys")
    assert inherited, "the TUI did not leave a server-owned PTY behind"
    assert any(item["status"] == "running" for item in inherited), inherited
    for item in inherited:
        request("DELETE", f"/v1/ptys/{item['id']}")

    created = request(
        "POST",
        "/v1/ptys",
        {
            "title": "e2e",
            "command": "/bin/sh",
            "args": [
                "-c",
                "printf 'WS_READY\\n'; read line; stty size; printf 'WS_ECHO:%s\\n' \"$line\"; sleep 30",
            ],
            "cols": 80,
            "rows": 24,
        },
    )
    pty_id = created["id"]
    ws_url = BASE.replace("http://", "ws://").replace("https://", "wss://")
    headers = {"Authorization": f"Bearer {TOKEN}"}

    async with websockets.connect(
        f"{ws_url}/v1/ptys/{pty_id}/connect?cursor=0",
        additional_headers=headers,
    ) as socket:
        output, cursor = await receive_until(socket, b"WS_READY")
        assert b"WS_READY" in output

    assert request("GET", f"/v1/ptys/{pty_id}")["status"] == "running"
    resized = request(
        "PATCH",
        f"/v1/ptys/{pty_id}",
        {"cols": 100, "rows": 40},
    )
    assert (resized["cols"], resized["rows"]) == (100, 40)

    async with websockets.connect(
        f"{ws_url}/v1/ptys/{pty_id}/connect?cursor={cursor}",
        additional_headers=headers,
    ) as socket:
        await socket.send(b"proof\n")
        output, _ = await receive_until(socket, b"WS_ECHO:proof")
        assert b"40 100" in output, output

    request("DELETE", f"/v1/ptys/{pty_id}")
    assert request("GET", "/v1/ptys") == []

    cleanup = request(
        "POST",
        "/v1/ptys",
        {
            "title": "shutdown-cleanup",
            "command": "/bin/sh",
            "args": ["-c", "sleep 30"],
            "cols": 80,
            "rows": 24,
        },
    )
    child_pid = cleanup["pid"]
    request("POST", "/v1/shutdown")
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            os.kill(child_pid, 0)
        except ProcessLookupError:
            break
        await asyncio.sleep(0.05)
    else:
        raise AssertionError(f"PTY child {child_pid} survived server shutdown")


asyncio.run(main())
