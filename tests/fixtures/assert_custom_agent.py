#!/usr/bin/env python3
"""Assert server-side custom-agent model resolution through the public API."""

import json
import sys
import urllib.error
import urllib.request


with open(sys.argv[1], encoding="utf-8") as runtime_file:
    runtime = json.load(runtime_file)

request = urllib.request.Request(
    f"{runtime['url']}/v1/threads",
    data=json.dumps(
        {"title": "ecosystem agent smoke", "agent": "ecosystem-smoke:reviewer"}
    ).encode(),
    headers={
        "Authorization": f"Bearer {runtime['token']}",
        "Content-Type": "application/json",
    },
    method="POST",
)
with urllib.request.urlopen(request, timeout=10) as response:
    thread = json.load(response)

assert thread["agent"] == "ecosystem-smoke:reviewer", thread
assert thread["provider"] == "echo", thread
assert thread["model"] == "echo-1", thread

request = urllib.request.Request(
    f"{runtime['url']}/v1/threads",
    data=json.dumps(
        {"title": "invalid primary", "agent": "ecosystem-smoke:worker"}
    ).encode(),
    headers={
        "Authorization": f"Bearer {runtime['token']}",
        "Content-Type": "application/json",
    },
    method="POST",
)
try:
    urllib.request.urlopen(request, timeout=10)
except urllib.error.HTTPError as error:
    assert error.code == 400, error
    assert "only as a subagent" in error.read().decode(), error
else:
    raise AssertionError("subagent-only agent was accepted as a primary thread")

print("custom agent model resolution passed")
