---
name: haarness-remote-access
description: Test Haarness Cloud remote hosts and published chats through the native connector, Chrome, Python/TypeScript SDKs and tmux, including reconnects, isolation and cleanup.
---

# Remote access verification

Read the native build skill and `docs/REMOTE-ACCESS.md` in the Cloud checkout. A remote host is a user-owned computer, not rented Cloud compute. Host mode controls advertised projects; published-session mode exposes one chat tree and its shared workspace files. Provider credentials and local server tokens stay on the computer. A Cloud API key is a separate identity.

Use a disposable native workspace and `HAARNESS_DATA_DIR` plus `HAARNESS_CLOUD_CONFIG_DIR`. Keep the user's daemons, projects and sessions intact. The CLI refuses old daemons without `remote-chat-scope.v1`; rebuild and restart only the fixture daemon. `--chat` publishes a chat; the global `--session` alias already means local thread selection.

## Automated/live checks

Regenerate matching Cloud and native SDK schemas as described in `../haarness-sdk-surface/SKILL.md`. Build the CLI and TypeScript SDK before the live script.

```sh
# Native checkout, SDK Python environment, local Cloud sandbox already running.
# Supply HAARNESS_API_KEY privately with remote:read, remote:connect, remote:control.
HAARNESS_REMOTE_E2E=1 HAARNESS_CLOUD_API_URL=http://127.0.0.1:19080 \
  PYTHONPATH=sdks/python/src sdks/python/.venv/bin/python scripts/test-remote-live.py
```

The script creates its own registry entries and isolated runtime, then revokes/stops them in cleanup. It covers actual CLI connection, discovery, hidden files, published children, denied sibling access, 10 MiB binary transfers, concurrent SDK reads, capacity exhaustion, accepted work during disconnect without replay, project close/reopen, native daemon replacement with address/token refresh, terminal idle keepalive, Python sync/async, TypeScript SSE and PTYs, session-only publication forgery denial, and background enable/status/disable/forget with explicit fresh registration. Its echo model is deterministic transport evidence, not live-provider proof. Evidence stays under `target/remote-e2e-*` without secrets.

Cloud `cargo test -p cloud-api --test remote` needs `HAARNESS_CLOUD_TEST_DATABASE_URL`; without it the database cases skip. Test two API replicas with distinct `HAARNESS_CLOUD_RELAY_ORIGIN` values. Verify independent consumer and publisher key revocation, account boundaries, cursor pages, reconnect generation fencing and abandoned streams. The native API suite tests the conversation-scope middleware. Compare actual suite output; merely running a command is not a pass.

## Chrome and tmux

Create a fixture host through the native CLI. In Chrome, open **Remote hosts**, then an advertised project. Check a real provider turn reads a known fixture file; verify hidden files, editing/upload, streaming history, terminal input/output, host rename and folder navigation. Upload testing may require the browser extension's file-URL permission; report that boundary if unavailable and retain API transfer proof separately.

Open the same native thread in a disposable tmux TUI. A prompt sent from Cloud should appear and stream there, and a local prompt should appear in Chrome. Stop only the fixture connector while retaining an unsent browser draft; it should show reconnecting without losing messages or remounting the composer. Restart and verify convergence. Stop/restart the sandbox API too: the event heartbeat watchdog must recover even if a proxy keeps the old response open. A native TUI reply after recovery must appear in Cloud without reloading the page. Publish one chat with `haarness remote run --chat CHAT_ID`; unrelated histories and host/terminal controls must be unavailable, while the published tree and shared workspace files remain usable.

For setup/recovery, exercise `remote run`, `enable`, `status`, `disable HOST_ID`, `hosts`, `sessions`, `revoke HOST_ID`, and `forget HOST_ID`. Disable stops transport, revoke removes Cloud access, and forget revokes a stopped identity before allocating a new one later. None deletes local files/history. Background enable does not install boot/login startup integration; foreground run can be supervised by the OS.

At completion, stop/revoke only owned fixtures, close owned terminal sessions, record exact source revisions and residual limitations, and leave a demo host running only when the user needs it for manual testing. Never save keys or local runtime tokens in reports or screenshots.
