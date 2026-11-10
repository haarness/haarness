#!/usr/bin/env python3
"""A stand-in for the macOS helper in tests: it answers each operation the way the real one would, writes every
request to $FAKE_HELPER_LOG, and never touches the screen.

Refs name fixed elements: e1 a Save button and e2 a Name field in TextEdit, e8 a password field in TextEdit, e9 a
Reload button and e11 the address field in Safari, e7 a button in Haarness itself, e6 text in Terminal, e5 Terminal's
New Tab button, e4 a button in Notes, e3 Notes' icon in the Dock, e10 the editor of Visual Studio Code, e14 its
terminal panel, e13 a switch in System Settings whose pane the helper cannot name, e15 a switch in its Privacy &
Security pane, e17 a switch in its Displays pane, e18 a switch owned by the Privacy & Security pane's own process,
e16 Terminal's Paste menu item, and e12 a Share button in Notes, which sits behind the person's window. The Save button names its
window and the toolbar around it, and a look at the focus for Meta+S names the menu item it would choose. Typing "PAUSE" pauses the
agent the way the bar would (and resumes half a second later); typing "HOLD" pauses it the same way until the server
sends "resume", as Resume pressed in a client does; typing "STOP" pauses it and then presses the bar's Stop; typing
"CRASH" pauses it and then quits, as a helper killed during a pause would; typing "ESCAPE" presses Escape and then
hangs, as an action still under way would. A look at pixel 13,13 is followed by Pause and then Stop on the bar, as
pressed while the approval for that spot waits. A close-up of TextEdit is trimmed to the screenshot, as the helper
does; a look at another app ignores zoom, as an older helper would.

Actions on e12 answer the way a helper that works behind the person's window does: a click went in behind, was
confirmed and opened and closed things; a scroll went in behind and showed no change; a drag and a hover came to the
front, saying why. Opening Mail starts it behind. Every other action answers as an older helper does, with none of
those fields. A look at Notes says it is behind, and one at Mail that its windows are minimized.
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
    1: {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXButton", "name": "Save", "content": False,
        "window": "Quarterly notes.rtf", "context": "Toolbar"},
    2: {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXTextField", "description": "Name", "content": False},
    8: {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXTextField", "subrole": "AXSecureTextField", "password": True, "content": False},
    9: {"bundle": "com.apple.Safari", "app": "Safari", "role": "AXButton", "name": "Reload", "content": True},
    7: {"bundle": "dev.haarness.desktop", "app": "Haarness", "role": "AXButton", "name": "Allow", "content": True},
    6: {"bundle": "com.apple.Terminal", "app": "Terminal", "role": "AXTextArea", "content": True},
    5: {"bundle": "com.apple.Terminal", "app": "Terminal", "role": "AXButton", "name": "New Tab", "content": False},
    4: {"bundle": "com.apple.Notes", "app": "Notes", "role": "AXButton", "name": "New Note", "content": False},
    3: {"bundle": "com.apple.dock", "app": "Dock", "role": "AXDockItem", "name": "Notes", "content": False},
    10: {"bundle": "com.microsoft.VSCode", "app": "Code", "role": "AXTextArea", "description": "Editor", "content": True},
    11: {"bundle": "com.apple.Safari", "app": "Safari", "role": "AXTextField", "description": "Address", "content": False},
    12: {"bundle": "com.apple.Notes", "app": "Notes", "role": "AXButton", "name": "Share", "content": False},
    13: {"bundle": "com.apple.systempreferences", "app": "System Settings", "role": "AXCheckBox", "name": "Bluetooth", "content": False},
    14: {"bundle": "com.microsoft.VSCode", "app": "Code", "role": "AXTextArea", "description": "Terminal 1", "content": True, "terminal": True},
    15: {"bundle": "com.apple.systempreferences", "app": "System Settings", "role": "AXCheckBox", "name": "Terminal", "content": False,
         "pane": "com.apple.settings.PrivacySecurity.extension", "context": "Automation"},
    16: {"bundle": "com.apple.Terminal", "app": "Terminal", "role": "AXMenuItem", "name": "Paste", "content": False, "paste": True},
    17: {"bundle": "com.apple.systempreferences", "app": "System Settings", "role": "AXCheckBox", "name": "Night Shift", "content": False,
         "pane": "com.apple.Displays-Settings.extension"},
    18: {"bundle": "com.apple.settings.PrivacySecurity.extension", "app": "Privacy & Security", "role": "AXCheckBox", "name": "Terminal",
         "content": False},
}

APPS = {"com.apple.TextEdit": "TextEdit", "com.apple.Notes": "Notes", "com.apple.Mail": "Mail"}

NOTES = {"bundle": "com.apple.Notes", "pid": 4, "name": "Notes"}

# How a helper that works behind the person's window answers an action on e12.
BEHIND = {
    "click": {
        "via": "ax",
        "background": True,
        "confirmed": True,
        "target": NOTES,
        "opened": [{"app": "Notes", "role": "sheet", "title": "Share\nNote"}, {"app": "Finder", "role": "menu"}],
        "closed": [{"app": "Notes", "role": "popover"}],
    },
    "scroll": {"via": "process", "background": True, "confirmed": False, "target": NOTES},
    "drag": {"via": "pointer", "background": False, "fallback": "drag", "target": NOTES},
    "hover": {"via": "pointer", "background": False, "fallback": "hover", "target": NOTES},
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
        names = {"com.apple.TextEdit": "TextEdit", "com.apple.Notes": "Notes", "com.apple.Terminal": "Terminal", "com.apple.Mail": "Mail"}
        if not bundle:
            return {"__error": ("not_found", "That app is not running.")}
        return {"bundle": bundle, "name": names.get(bundle, bundle), "running": bundle == "com.apple.TextEdit"}
    if op == "snapshot":
        bundle = request.get("bundle") if request.get("bundle") in APPS else "com.apple.TextEdit"
        reply = {
            "size": {"width": 100, "height": 50, "factor": 1},
            "windows": [{"id": 1, "pid": 2, "app": "TextEdit", "bundle": "com.apple.TextEdit", "layer": 0, "title": "Untitled", "frame": {"x": 0, "y": 0, "width": 100, "height": 50}}],
            "frontmost": {"pid": 2, "bundle": "com.apple.TextEdit", "name": "TextEdit"},
            "app": {"pid": 2, "bundle": bundle, "name": APPS[bundle]},
            "focused": {"bundle": "com.apple.TextEdit", "app": "TextEdit", "role": "AXTextField", "description": "Name"},
            "outline": '@e1 [button] "Save" at=10,10 size=20x10\n@e2 [text-field] "" description="Name" at=40,10 size=40x10',
            "image": JPEG,
            "accessibility": True,
            "whole": bool(request.get("whole")),
        }
        if request.get("zoom") and bundle == "com.apple.TextEdit":
            zoom = dict(request["zoom"])
            zoom["width"] = min(zoom["width"], 100 - zoom["x"])
            zoom["height"] = min(zoom["height"], 50 - zoom["y"])
            reply["zoom"] = zoom
        if bundle == "com.apple.Notes":
            reply["behind"] = True
        if bundle == "com.apple.Mail":
            reply["offscreen"] = "minimized"
        return reply
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
        element = dict(ELEMENTS[2], password=False)
        if "Meta+S" in str(request.get("keys", "")):
            element["menu"] = "File ▸ Save…"
        return element
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
    if op == "launch" and request.get("bundle") == "com.apple.Mail":
        return {"via": "process", "background": True, "target": {"bundle": "com.apple.Mail", "pid": 5, "name": "Mail"}}
    if op in BEHIND and 12 in (request.get("ref"), (request.get("from") or {}).get("ref")):
        return BEHIND[op]
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
