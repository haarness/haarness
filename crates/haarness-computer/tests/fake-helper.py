#!/usr/bin/env python3
"""A stand-in for the macOS helper in tests: it answers each operation the way the real one would, writes every
request to $FAKE_HELPER_LOG, and never touches the screen.

Refs name fixed elements: e1 a Save button and e2 a Name field in TextEdit, e8 a password field in TextEdit, e9 a
Reload button in Safari, e7 a button in Haarness itself, e6 text in Terminal, e5 Terminal's New Tab button, e4 a
button in Notes. Typing "PAUSE" pauses the agent the way the bar would (and resumes half a second later); typing
"HOLD" pauses it the same way until the server sends "resume", as Resume pressed in a client does; typing "STOP" pauses
it and then presses the bar's Stop; typing "CRASH" pauses it and then quits, as a helper killed during a pause would;
typing "ESCAPE" presses Escape and then hangs, as an action still under way would. A look at pixel 13,13 is followed by
Pause and then Stop on the bar, as pressed while the approval for that spot waits.
"""
import json
import os
import sys
import threading
import time

# An 8x8 grey JPEG.
JPEG = (
    "/9j/4AAQSkZJRgABAQAASABIAAD/4QBMRXhpZgAATU0AKgAAAAgAAYdpAAQAAAABAAAAGgAAAAAAA6ABAAMAAAABAAEAAKACAAQAAAABAAAA"
    "CKADAAQAAAABAAAACAAAAAD/7QA4UGhvdG9zaG9wIDMuMAA4QklNBAQAAAAAAAA4QklNBCUAAAAAABDUHYzZjwCyBOmACZjs+EJ+/8AAEQgA"
    "CAAIAwEiAAIRAQMRAf/EAB8AAAEFAQEBAQEBAAAAAAAAAAABAgMEBQYHCAkKC//EALUQAAIBAwMCBAMFBQQEAAABfQECAwAEEQUSITFBBhNR"
    "YQcicRQygZGhCCNCscEVUtHwJDNicoIJChYXGBkaJSYnKCkqNDU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6g4SFhoeI"
    "iYqSk5SVlpeYmZqio6Slpqeoqaqys7S1tre4ubrCw8TFxsfIycrS09TV1tfY2drh4uPk5ebn6Onq8fLz9PX29/j5+v/EAB8BAAMBAQEBAQEB"
    "AQEAAAAAAAABAgMEBQYHCAkKC//EALURAAIBAgQEAwQHBQQEAAECdwABAgMRBAUhMQYSQVEHYXETIjKBCBRCkaGxwQkjM1LwFWJy0QoWJDTh"
    "JfEXGBkaJicoKSo1Njc4OTpDREVGR0hJSlNUVVZXWFlaY2RlZmdoaWpzdHV2d3h5eoKDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2"
    "t7i5usLDxMXGx8jJytLT1NXW19jZ2uLj5OXm5+jp6vLz9PX29/j5+v/bAEMAAgICAgICAwICAwUDAwMFBgUFBQUGCAYGBgYGCAoICAgICAgK"
    "CgoKCgoKCgwMDAwMDA4ODg4ODw8PDw8PDw8PD//bAEMBAgICBAQEBwQEBxALCQsQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ"
    "EBAQEBAQEBAQEBAQEBAQEP/dAAQAAf/aAAwDAQACEQMRAD8AKKKKAP/Z"
)
LOG = os.environ.get("FAKE_HELPER_LOG", "")
LOCK = threading.Lock()


def send(value):
    with LOCK:
        sys.stdout.write(json.dumps(value) + "\n")
        sys.stdout.flush()


ELEMENTS = {
    1: {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXButton", "name": "Save", "content": False},
    2: {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXTextField", "description": "Name", "content": False},
    8: {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXTextField", "subrole": "AXSecureTextField", "password": True, "content": False},
    9: {"bundle": "com.apple.Safari", "app": "Safari", "role": "AXButton", "name": "Reload", "content": True},
    7: {"bundle": "dev.haarness.desktop", "app": "Haarness", "role": "AXButton", "name": "Allow", "content": True},
    6: {"bundle": "com.apple.Terminal", "app": "Terminal", "role": "AXTextArea", "content": True},
    5: {"bundle": "com.apple.Terminal", "app": "Terminal", "role": "AXButton", "name": "New Tab", "content": False},
    4: {"bundle": "com.apple.Notes", "app": "Notes", "role": "AXButton", "name": "New Note", "content": False},
}


def answer(op, request):
    if op == "hello":
        return {"protocol": 1, "build": "test", "pid": os.getpid()}
    if op in ("status", "prompt"):
        return {"accessibility": True, "screenRecording": True, "console": True, "locked": False, "asleep": False, "macos": "15.0"}
    if op == "apps":
        return {"apps": [
            {"pid": 2, "name": "TextEdit", "bundle": "com.apple.TextEdit", "active": True, "hidden": False},
            {"pid": 3, "name": "1Password", "bundle": "com.1password.1password", "active": False, "hidden": False},
        ]}
    if op == "identify":
        bundle = request.get("bundle") or {"TextEdit": "com.apple.TextEdit", "Notes": "com.apple.Notes"}.get(request.get("name"), "")
        names = {"com.apple.TextEdit": "TextEdit", "com.apple.Notes": "Notes", "com.apple.Terminal": "Terminal"}
        if not bundle:
            return {"__error": ("not_found", "That app is not running.")}
        return {"bundle": bundle, "name": names.get(bundle, bundle), "running": bundle == "com.apple.TextEdit"}
    if op == "snapshot":
        return {
            "size": {"width": 100, "height": 50, "factor": 1},
            "windows": [{"id": 1, "pid": 2, "app": "TextEdit", "bundle": "com.apple.TextEdit", "layer": 0, "title": "Untitled", "frame": {"x": 0, "y": 0, "width": 100, "height": 50}}],
            "frontmost": {"pid": 2, "bundle": "com.apple.TextEdit", "name": "TextEdit"},
            "app": {"pid": 2, "bundle": "com.apple.TextEdit", "name": "TextEdit"},
            "focused": {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXTextField", "description": "Name"},
            "outline": '@e1 [button] "Save" at=10,10 size=20x10\n@e2 [text-field] "" description="Name" at=40,10 size=40x10',
            "image": JPEG,
            "accessibility": True,
            "whole": bool(request.get("whole")),
        }
    if op == "inspect":
        if "ref" in request:
            element = dict(ELEMENTS.get(int(request["ref"]), ELEMENTS[1]))
            element.setdefault("password", False)
            element["point"] = {"x": 20, "y": 15}
            if request.get("preview"):
                element["preview"] = JPEG
            return element
        if "x" in request:
            if request["x"] == 13:
                threading.Timer(0.3, lambda: send({"event": "pause"})).start()
                threading.Timer(0.8, lambda: send({"event": "stop"})).start()
            return {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXTextArea", "content": True, "point": {"x": request["x"], "y": request["y"]}}
        return dict(ELEMENTS[2], password=False)
    if op == "type" and request.get("text") == "PAUSE":
        send({"event": "pause"})
        threading.Timer(0.5, lambda: send({"event": "resume"})).start()
        return {"__error": ("paused", "Paused by the person after 0 of 5 characters.")}
    if op == "type" and request.get("text") in ("HOLD", "STOP", "CRASH"):
        # The press reaches the server just after the refusal here, as it can when the server takes the two in out of
        # order.
        threading.Timer(0.1, lambda: send({"event": "pause"})).start()
        if request["text"] == "STOP":
            threading.Timer(0.6, lambda: send({"event": "stop"})).start()
        if request["text"] == "CRASH":
            threading.Timer(0.6, lambda: os._exit(9)).start()
        return {"__error": ("paused", "Paused by the person after 0 of 4 characters.")}
    if op == "type" and request.get("text") == "ESCAPE":
        send({"event": "escape"})
        time.sleep(30)
        return {}
    return {"via": "test"}


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    request = json.loads(line)
    if LOG:
        with open(LOG, "a") as log:
            log.write(line + "\n")
    op = request.get("op")
    if op in ("release", "resume"):
        send({"id": request.get("id"), "ok": True, "data": {}})
        continue
    result = answer(op, request)
    if isinstance(result, dict) and "__error" in result:
        code, message = result["__error"]
        send({"id": request.get("id"), "ok": False, "code": code, "message": message})
    else:
        send({"id": request.get("id"), "ok": True, "data": result})
