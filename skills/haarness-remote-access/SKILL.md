---
name: haarness-remote-access
description: Test Haarness Cloud remote hosts and published remote sessions (native chat trees) through the CLI connector, Chrome, tmux TUI and Python/TypeScript SDKs. Use for remote access, sharing, relay, reconnect, scope, stress and cleanup changes; distinguish these from rented Cloud sessions.
---

# Remote access verification

Read the [native build skill](../../.skills/haarness-build-run/SKILL.md) and `docs/REMOTE-ACCESS.md` in the matching Cloud checkout. For browser/TUI, authorization, failure injection and cleanup, follow the [remote-session test plan](references/remote-session-test-plan.md). Previous verification reports describe historical runs, not current passes.

## Identify the surface and IDs

| Concept | Meaning and test boundary |
| --- | --- |
| Rented Cloud session | Cloud-managed machine; use the [sessions skill](../haarness-cloud-sessions-chats-schedules/SKILL.md) for its compute lifecycle |
| Remote host ID | User-owned computer registration and outbound WebSocket connector; advertises native projects |
| Native project ID | Read from the host's project catalog; selects a workspace/filesystem containing multiple chats |
| Native thread/chat ID | Conversation history inside that project; used by the TUI and native runtime APIs |
| Remote session/publication ID | Cloud sharing record for a native chat tree; its `threadId` is the native chat ID, not the publication ID |

Published access includes descendants and the shared project filesystem. It excludes sibling histories, new/forked chats, host management and PTYs; it is not a filesystem sandbox for one chat. Provider credentials and local server tokens stay on the computer. Cloud control credentials must never become model-provider credentials.

## Prepare an isolated fixture

- Record native/Cloud source SHAs, selected Cloud checkout, API/web addresses and binary path. Use current endpoint discovery; daemon replacement changes its address/token.
- Use an owned disposable workspace plus `HAARNESS_DATA_DIR` and `HAARNESS_CLOUD_CONFIG_DIR`. Keep user daemons/projects intact. Retain a manifest of host/publication/project/thread/PTY IDs and fixture processes.
- Reuse the local Cloud sandbox with migrated PostgreSQL. The CLI requires runtime capability `remote-chat-scope.v1`; if stale, rebuild and restart only the fixture daemon.
- Supply a private test key with `remote:read`, `remote:connect`, `remote:control` and an appropriate account role. Model access is configured separately on the native fixture. Never print tokens or save them in evidence.
- If contracts changed, regenerate matching schemas using the [SDK skill](../haarness-sdk-surface/SKILL.md). Build the CLI and TypeScript SDK when their code changed or outputs are absent.

## Focused and live automated checks

Run from the native repository:

```sh
cargo test -p haarness-cli --bin haarness remote_cloud::tests
cargo test -p haarness-server --test api published_remote_chat_cannot_read_or_control_other_conversations
npm --prefix sdks/typescript run build
npm --prefix sdks/typescript test
sdks/python/.venv/bin/python -m pytest sdks/python/tests -q
```

Broaden to `cargo test -p haarness-server --test api` for shared server/auth/event changes. For Cloud changes, run `cargo test -p cloud-api --test remote` in that checkout with private `HAARNESS_CLOUD_TEST_DATABASE_URL` configured. Its database cases skip without the variable: a skipped suite is not relay proof. Web changes also need that checkout's `npm --prefix web run typecheck`, `npm --prefix web test` and `npm --prefix web run build`.

```sh
# Native checkout, SDK Python environment, local Cloud sandbox already running.
# HAARNESS_API_KEY is already set privately; optional HAARNESS_BIN selects the CLI.
HAARNESS_REMOTE_E2E=1 HAARNESS_CLOUD_API_URL=http://127.0.0.1:19080 \
  PYTHONPATH=sdks/python/src sdks/python/.venv/bin/python scripts/test-remote-live.py
```

Read [the live script](../../scripts/test-remote-live.py) before running. It is loopback-only; do not relax that guard to stress production. It owns its runtime/registrations and cleanup, and invokes [the TypeScript integration](../../sdks/tests/remote_e2e.mjs). Coverage includes:

- Discovery/hidden files, published root/children and denied siblings/host/terminal operations.
- 10 MiB binary equality, 200 reads at concurrency 20, 32 held streams then capacity recovery.
- Approved work completing through a disconnect without duplicate effects; project close/reopen and native daemon replacement with fresh endpoint/token discovery.
- PTY idle keepalive beyond 40 seconds, Python sync/async and TypeScript HTTP/events/PTYs.
- Session-only publication forgery denial and CLI enable/status/disable/forget/fresh registration.

Check exit status, individual checks and cleanup. The echo provider is deterministic transport evidence; it does not prove a real model, browser interaction or physical file picker. Secret-free results remain under `target/remote-e2e-*/checks.json`.

## Complete the user journey

Follow the [test plan](references/remote-session-test-plan.md) for full-host and published-chat Chrome control, real-model file reading, bidirectional tmux chat updates, permissions/questions, reconnect with unsent draft retention, cross-replica routing and independent credential revocation. A connected badge or successful click alone is not a pass: verify fresh runtime effects and authoritative history after recovery.

Use `remote run --chat THREAD_ID` to publish. The global `--session` flag is an alias for native thread selection, not publication. Background `remote enable` does not install boot/login startup integration.

Stop sharing/revoke/disable only owned fixtures and close their streams, PTYs and tmux sessions. Project close, unpublish and host revocation must retain native files/history; never apply rented-session deletion to remote hosts. Report exact revisions, automated/Chrome/tmux/provider evidence separately, cleanup status and any untested boundary. Leave a demo running only when needed for user testing.
