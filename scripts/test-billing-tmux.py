#!/usr/bin/env python3
"""Opt-in real Cloud router debit and live TUI billing proof using an existing login.

HAARNESS_BILLING_LIVE=1 HAARNESS_CLOUD_API_URL=... HAARNESS_CLOUD_CONFIG_DIR=...
Creates only a disposable local project/thread. Makes one small, paid model call.
"""
from decimal import Decimal, ROUND_DOWN
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request

if os.environ.get("HAARNESS_BILLING_LIVE") != "1":
    raise SystemExit("Opt in with HAARNESS_BILLING_LIVE=1; this test consumes model credits.")
for key in ["HAARNESS_CLOUD_API_URL", "HAARNESS_CLOUD_CONFIG_DIR"]:
    if not os.environ.get(key):
        raise SystemExit(f"{key} is required")
binary = str(Path(os.environ.get("HAARNESS_BIN", "target/debug/haarness")).resolve())
model = os.environ.get("HAARNESS_BILLING_MODEL", "openai/gpt-4.1-nano")
evidence = Path(os.environ.get("HAARNESS_BILLING_EVIDENCE", "target/billing-tmux"))
evidence.mkdir(parents=True, exist_ok=True)


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=120):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


def wallet():
    return json.loads(run([binary, "account", "--rpc"], input='{"action":"billing"}'))


with tempfile.TemporaryDirectory(prefix="haarness-billing-tmux-") as directory:
    root = Path(directory).resolve()
    workspace = root / "project"
    (workspace / ".haarness").mkdir(parents=True)
    (workspace / ".haarness/config.toml").write_text(
        f'[defaults]\nprovider="haarness"\nmodel="{model}"\n[lsp]\nenabled=false\n'
    )
    run(["git", "init", "-q", str(workspace)])
    env = dict(os.environ, HAARNESS_DATA_DIR=str(root / "data"))
    args = [binary, "--workspace", str(workspace), "--trust-project"]
    endpoint = json.loads(run(args + ["desktop"], input='{"action":"connect"}', env=env))

    def api(path, body=None):
        req = urllib.request.Request(endpoint["url"] + "/v1" + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization":"Bearer " + endpoint["token"], "Content-Type":"application/json"})
        with urllib.request.urlopen(req, timeout=30) as response:
            return json.load(response)

    thread = api("/threads", {"title":"Cloud billing proof", "provider":"haarness", "model":model})["id"]
    session = "haarness-billing-" + str(os.getpid())

    def capture():
        return run(["tmux", "capture-pane", "-p", "-t", session])

    def send(text):
        run(["tmux", "send-keys", "-l", "-t", session, text])
        run(["tmux", "send-keys", "-t", session, "Enter"])

    try:
        command = shlex.join([*args, "--thread", thread])
        launcher = root / "tui-env.sh"
        launcher.write_text("\n".join("export "+k+"="+shlex.quote(v) for k,v in env.items())+"\nexec "+command+"\n")
        launcher.chmod(0o600)
        run(["tmux", "new-session", "-d", "-x", "160", "-y", "46", "-s", session, "bash "+shlex.quote(str(launcher))])
        until(lambda: "/billing" in capture(), "Cloud credit indicator missing")
        send("/billing")
        until(lambda: "Purchased" in capture() and "Included AI" in capture(), "Billing panel missing")
        (evidence / "before.txt").write_text(capture())
        before = wallet()
        assert before["heldReserve"]["nanos"] == "0", "Test requires an idle billing account"
        run(["tmux", "send-keys", "-t", session, "Escape"])
        send("Without tools, reply with exactly TUI_BILLING_OK")
        until(lambda: any("TUI_BILLING_OK" in i.get("data", {}).get("text", "") and i.get("data",{}).get("type") == "assistant_message" for i in api(f"/threads/{thread}/items")) and not api("/turns/active"), "Cloud model did not finish")

        def settled():
            after = wallet()
            if after["heldReserve"]["nanos"] != "0" or any(b["reserved"] != "0" for b in after["buckets"]):
                return None
            return after if int(after["includedAi"]["nanos"]) < int(before["includedAi"]["nanos"]) else None

        after = until(settled, "Cloud request did not settle")
        assert before["purchased"]["nanos"] == after["purchased"]["nanos"], "Included credits should pay first"
        send("/billing")
        display = f'{Decimal(after["includedAi"]["credits"]).quantize(Decimal("0.01"), rounding=ROUND_DOWN):,}'.rstrip("0").rstrip(".")
        until(lambda: display in capture(), "Live balance did not reach TUI")
        (evidence / "after.txt").write_text(capture())
        (evidence / "wallet.json").write_text(json.dumps({"before":before,"after":after},indent=2))
        for width in [100, 80]:
            run(["tmux", "resize-window", "-t", session, "-x", str(width), "-y", "46"])
            until(lambda: "Included AI" in capture(), f"Billing panel lost at {width} columns")
            (evidence / f"width-{width}.txt").write_text(capture())
        print("PASS: real Cloud model, included-credit debit, released reserves, live TUI billing and narrow layouts")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True)
