#!/usr/bin/env python3
"""Real-model native read and attached-TUI upload of an image above ten MiB."""
import json
import os
from pathlib import Path
import shlex
import struct
import subprocess
import sys
import tempfile
import time
from uuid import uuid4
import zlib

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "sdks/tests"))
from server import Fixture, BINARY
from haarness_sdk import Haarness

evidence = Path(tempfile.mkdtemp(prefix="haarness-media-parity-live-"))
model = os.environ.get("HAARNESS_PARITY_MODEL", "gpt-5.6-luna")
fixture = Fixture()
fixture.env["OPENAI_API_KEY"] = os.environ.get("OPENAI_API_KEY") or json.loads(
    (Path.home() / ".local/share/haarness/auth.json").read_text()
)["openai"]["key"]


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def until(check, label, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.2)
    raise AssertionError("Timed out: " + label)


session = "haarness-media-" + uuid4().hex[:10]


def tmux(*args, input=None):
    return subprocess.run(
        ["tmux", *args], input=input, text=True, capture_output=True, check=True, timeout=15
    ).stdout


try:
    # A valid red PNG with a legal ancillary text chunk exercises actual byte
    # transport without requiring a huge decoded image or model token budget.
    image = fixture.workspace / "large-red.png"
    image.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 64, 8, 2, 0, 0, 0))
        + chunk(b"tEXt", b"fixture\x00" + b"A" * (11 * 1024 * 1024))
        + chunk(b"IDAT", zlib.compress((b"\x00" + b"\xff\x00\x00" * 64) * 64))
        + chunk(b"IEND", b"")
    )
    with fixture, Haarness(fixture.url, token=fixture.token, timeout=60) as client:
        native = client.create_thread({"title": "Large native media proof", "provider": "openai", "model": model})
        turn = client.start_turn(native["id"], {
            "requestId": str(uuid4()),
            "text": "Use the read tool to inspect large-red.png. Then identify the dominant color from the image and answer with that color. Do not use shell, code or external tools.",
        })
        completed = until(lambda: (value if (value := client.get_turn(turn["id"]))["status"] not in ["running", "queued"] else None), "native image read")
        assert completed["status"] == "completed", completed
        items = client.list_items(native["id"])
        assert any(i["data"]["type"] == "tool_result" and i["data"]["name"] == "read" and i["data"]["is_error"] is False for i in items)
        assert any(i["data"]["type"] == "assistant_message" and "red" in i["data"]["text"].lower() for i in items)
        (evidence / "native-items.json").write_text(json.dumps(items, indent=2))
        print("PASS real model read the eleven MiB PNG and identified red", flush=True)

        attached = client.create_thread({"title": "Large TUI upload proof", "provider": "openai", "model": model})
        command = shlex.join([
            "env", "HAARNESS_SERVER_TOKEN=" + fixture.token,
            "HAARNESS_HOME=" + str(fixture.home),
            str(BINARY), "attach", fixture.url, "--thread", attached["id"],
        ])
        tmux("new-session", "-d", "-s", session, "-x", "140", "-y", "42", command)
        capture = lambda: tmux("capture-pane", "-p", "-t", session)
        until(lambda: "openai" in capture().lower(), "TUI ready")
        tmux("load-buffer", "-b", session, "-", input=str(image))
        tmux("paste-buffer", "-p", "-b", session, "-t", session)
        until(lambda: "large-red.png" in capture(), "large media drop")
        (evidence / "tui-draft.txt").write_text(capture())
        tmux("send-keys", "-t", session, "-l", "Identify the dominant color in the attached image. Answer with that color only.")
        tmux("send-keys", "-t", session, "Enter")
        # Poll compact turn state; repeatedly downloading a sixteen-MiB
        # transcript would make this test itself a substantial load generator.
        active = until(lambda: client.active_turn(attached["id"]), "uploaded turn admission")
        completed = until(lambda: (value if (value := client.get_turn(active["id"]))["status"] not in ["running", "queued"] else None), "uploaded image response", timeout=240)
        assert completed["status"] == "completed", completed
        items = client.list_items(attached["id"])
        assert any(i["data"]["type"] == "assistant_message" and "red" in i["data"]["text"].lower() for i in items)
        user = next(i["data"] for i in items if i["data"]["type"] == "user_message")
        assert user["attachments"][0].startswith("data:image/png;")
        assert len(user["attachments"][0]) > 14 * 1024 * 1024
        (evidence / "tui-items.json").write_text(json.dumps(items, indent=2))
        (evidence / "tui-finished.txt").write_text(capture())
        result = {"model": model, "bytes": image.stat().st_size, "nativeRead": "red", "attachedTuiUpload": "red"}
        (evidence / "results.json").write_text(json.dumps(result, indent=2))
        print("PASS real-model native media read and attached TUI upload:", json.dumps(result), flush=True)
finally:
    try:
        (evidence / "tui-last.txt").write_text(tmux("capture-pane", "-p", "-t", session))
    except subprocess.CalledProcessError:
        pass
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    subprocess.run(["tmux", "delete-buffer", "-b", session], capture_output=True)
    fixture.close()
    print("Evidence retained:", evidence, flush=True)
