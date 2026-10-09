#!/usr/bin/env python3
"""Opt-in real router selection and credit isolation through the TUI.

HAARNESS_BILLING_LIVE=1 HAARNESS_CLOUD_API_URL=... HAARNESS_CLOUD_CONFIG_DIR=...
Creates only a disposable local project/thread. Makes one small, paid model call.
"""
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
evidence = Path(os.environ.get("HAARNESS_BILLING_EVIDENCE", "target/router-profiles-tmux"))
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
        until(lambda: "/billing" in capture(), "Cloud indicator missing")
        routers = json.loads(run([binary, "account", "--routers"]))["items"]
        wanted = os.environ["HAARNESS_ROUTER_TEST_ID"]
        byok = next(r for r in routers if r["id"] == wanted)
        default = next(r for r in routers if r["isDefault"])
        for row in [byok, default]:
            send("/router")
            until(lambda: "Cloud router on this computer" in capture(), "Router picker missing")
            (evidence / (row["credentialMode"] + "-picker.txt")).write_text(capture())
            # The picker starts at row zero and preserves the server's displayed order.
            for _ in range(routers.index(row)):
                run(["tmux", "send-keys", "-t", session, "Down"])
            run(["tmux", "send-keys", "-t", session, "Enter"])
            until(lambda: json.loads(run([binary,"account","--json"])).get("selectedRouterId") == row["id"], "Selection did not persist")
            before = wallet()
            assert before["heldReserve"]["nanos"] == "0"
            marker = "TUI_ROUTER_" + row["credentialMode"].upper() + "_OK"
            send("Without tools, reply with exactly " + marker)
            until(lambda: any(marker in i.get("data",{}).get("text","") and i.get("data",{}).get("type")=="assistant_message" for i in api(f"/threads/{thread}/items")) and not api("/turns/active"), "Selected router did not answer")
            # Runtime completion precedes asynchronous router settlement slightly.
            time.sleep(1)
            after = wallet()
            assert after["heldReserve"]["nanos"] == "0"
            if row["credentialMode"] == "byok":
                assert before["includedAi"] == after["includedAi"] and before["purchased"] == after["purchased"], "BYOK debited Cloud credits"
            else:
                until(lambda: int(wallet()["includedAi"]["nanos"]) < int(before["includedAi"]["nanos"]), "System router did not debit credits")
            (evidence / (row["credentialMode"] + "-reply.txt")).write_text(capture())
        for width in [100,80]:
            run(["tmux","resize-window","-t",session,"-x",str(width),"-y","46"])
            send("/router")
            until(lambda: byok["name"] in capture(), "Router picker clipped")
            (evidence / f"router-width-{width}.txt").write_text(capture())
            run(["tmux","send-keys","-t",session,"Escape"])
        print("PASS: tmux router selection, persistent preference, real BYOK zero debit and system debit, narrow layouts")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True)
