#!/usr/bin/env python3
"""Actual CLI/server preview lifecycle, containment and compatibility checks. No external services."""
import hashlib
import json
from pathlib import Path
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sdks/tests"))
from server import Fixture


def request(server, path, body=None, method=None, status=200):
    req = urllib.request.Request(server.url + path, data=json.dumps(body).encode() if body is not None else None,
        method=method, headers={"Authorization": "Bearer " + server.token, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            assert response.status == status, response.status
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code != status:
            raise AssertionError((error.code, error.read().decode())) from error
        return error.read().decode()


def tool(server, path, *, preview=True, queue=False, mode="preview"):
    thread = request(server, "/v1/threads", {"provider": "echo", "model": "echo-1"}, status=201)
    body = {"requestId": str(uuid.uuid4()), "text": '!tool preview ' + json.dumps({"filePath": str(path), "mode": mode})}
    if queue:
        body.update(delivery="queue", preview=preview)
        result = request(server, f'/v1/threads/{thread["id"]}/input', body, status=202)
        turn = result["turn"]
    else:
        if preview: body["tools"] = {"preview": True}
        turn = request(server, f'/v1/threads/{thread["id"]}/turns', body, status=202)
    request(server, f'/v1/turns/{turn["id"]}/wait?timeout_ms=20000')
    items = request(server, f'/v1/threads/{thread["id"]}/items')
    return next(item["data"] for item in reversed(items) if item["data"]["type"] == "tool_result")


def version(server, path, hash, **options):
    return request(server, '/v1/files/preview?' + urllib.parse.urlencode({"path":path, "hash":hash}), **options)


with Fixture() as server:
    page = server.workspace / "canvas.html"
    first, second, third = "<h1>first</h1>", "<h1>second</h1>", "<h1>unsaved-by-agent</h1>"
    page.write_text(first)
    denied = tool(server, "canvas.html", preview=False)
    assert denied["is_error"], "Legacy/TUI clients must not be allowed to call preview"
    output = tool(server, "canvas.html")
    assert not output["is_error"], output
    hash = output["metadata"]["preview"]["version"]
    assert version(server, "canvas.html", hash)["content"] == first
    assert not tool(server, page, mode="code")["is_error"]
    assert not tool(server, "canvas.html", queue=True)["is_error"], "Queued turns retain preview support"
    assert tool(server, "canvas.html", preview=False, queue=True)["is_error"]
    for path in ("../outside.html", "missing.html", ".git/config", "README.txt"):
        assert tool(server, path)["is_error"], path
    outside = server.root / "outside.html"; outside.write_text("private")
    (server.workspace / "escape.html").symlink_to(outside)
    assert tool(server, "escape.html")["is_error"]
    page.write_text(second)
    second_hash = hashlib.sha256(second.encode()).hexdigest()
    result = request(server, '/v1/files/preview/restore', {"path":"canvas.html", "hash":hash, "revision":second_hash})
    assert page.read_text() == first
    assert version(server, "canvas.html", result["backup"])["content"] == second
    page.write_text(third)
    request(server, '/v1/files/preview/restore', {"path":"canvas.html", "hash":hash, "revision":second_hash}, status=409)
    assert page.read_text() == third
    page.write_bytes(b'x' * (5 * 1024 * 1024 + 1))
    assert tool(server, "canvas.html")["is_error"]
    request(server, '/v1/files/preview/restore', {"path":"canvas.html", "hash":hash, "revision":hashlib.sha256(page.read_bytes()).hexdigest()}, status=413)
    assert page.stat().st_size > 5 * 1024 * 1024
    page.unlink()
    request(server, '/v1/files/preview/restore', {"path":"canvas.html", "hash":hash, "revision":hashlib.sha256(b'').hexdigest()})
    assert page.read_text() == first
    request(server, '/v1/files/preview/restore', {"path":"escape.html", "hash":hash, "revision":hashlib.sha256(b'private').hexdigest()}, status=400)
    assert outside.read_text() == 'private'
    version(server, "canvas.html", "../secret", status=400)
    with Fixture() as other:
        (other.workspace / "canvas.html").write_text(second)
        version(other, "canvas.html", hash, status=404)
    assert not (server.workspace / '.haarness/preview').exists()
    print('PASS: real tool events, opt-in gating, queued delivery, immutable versions, absolute paths, containment, restore/backup, conflicts, size limits, deletion recovery, cross-workspace isolation')
