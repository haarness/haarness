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
- Supply a private test key with `remote:read`, `remote:connect`, `remote:control`, `workspaces:read` and `workspaces:write` (the live script lists and dismisses attention) and an appropriate account role. Model access is configured separately on the native fixture. Never print tokens or save them in evidence.
- If contracts changed, regenerate matching schemas using the [SDK skill](../haarness-sdk-surface/SKILL.md). Build the CLI and TypeScript SDK when their code changed or outputs are absent.

## Focused and live automated checks

Run from the native repository:

```sh
cargo test -p haarness-cli --bin haarness remote_cloud::tests
cargo test -p haarness-cli --bin haarness remote_attention::tests
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
- 10 MiB binary equality, 200 reads at concurrency 20, 48 held streams beside a request (no per-host cap), then reader cleanup.
- Approved work completing through a disconnect without duplicate effects; project close/reopen and native daemon replacement with fresh endpoint/token discovery.
- PTY idle keepalive beyond 40 seconds, Python sync/async and TypeScript HTTP/events/PTYs.
- Session-only publication forgery denial and CLI enable/status/disable/forget/fresh registration.
- Attention (`GET /api/v1/attention`): an approval with its category and chat title but no command text, resolved by the answer; a helper agent's approval under its parent chat, and one finished row for the parent's turn only; a second project's question while its server runs, withdrawn while the project is closed, and its stopped server never restarted by watching; a project server from another Haarness build (a cloned binary) reported with `actionable: false` and refused through Cloud; a network drop (connectors reach Cloud through a proxy the script cuts) and a connector restart keeping each open row without a gap, offline meanwhile, with an answer given offline and a turn finished offline reported once; a `/remote-control` share beside the host-mode connector reported by both; a chat-only connector reporting only its tree, with `remoteSessionId`.
- Push notifications, when Cloud runs the file provider: set `HAARNESS_PUSH_DIRECTORY` to its folder and `HAARNESS_PUSH_BEARER` to a native app access token of the key's user (API keys cannot register a phone). The script registers a fake device, checks each alert's wording keys, badge and helper-agent link, and that every request and finish was announced exactly once, including the request both connectors report, then removes the device.

Check exit status, individual checks and cleanup. The echo provider is deterministic transport evidence; it does not prove a real model, browser interaction or physical file picker. Secret-free results remain under `target/remote-e2e-*/checks.json`.

## `/remote-control` from the TUI and Desktop

`/remote-control` (alias `/rc`; `status`, `stop`) shares the open chat through the server's `remote-control` management resource, which starts the same connector as `remote enable --chat` and returns the link plus QR rows. Unit coverage: `cargo test -p haarness-cli --bin haarness remote_cloud::tests remote_control_qr`, `cargo test -p haarness-tui --lib remote_control`, `npm --prefix apps/desktop test` (`tests/remote-control.test.ts`). Live, against the local Cloud sandbox:

- TUI in a private tmux socket with isolated `HAARNESS_DATA_DIR`/`HAARNESS_CLOUD_CONFIG_DIR`, `HAARNESS_CLOUD_API_URL` and `HAARNESS_CLOUD_WEB_ORIGIN`, `SSH_TTY=fixture`: type `/rc` while signed out, Enter signs in with a device code (approve it on the web's `/device` page), and sharing resumes by itself. Decode the terminal QR from `tmux capture-pane -e` colours with OpenCV and compare it with the link.
- Open the link in a browser signed in to the same account, send from there, and confirm the turn appears in the TUI; the band shows `remote`.
- `/rc stop` must leave no connector, no `remote-*` identity files and no host/publication in Cloud; the old link returns 404. Stopping from the Cloud page must clear the TUI badge within a minute and clean up the connector the same way. Sharing again gives a new link.
- Desktop: `npx playwright test e2e/remote-control.spec.ts` (signed-out case always; the live case with `HAARNESS_REMOTE_CONTROL_API`, `_WEB` and a signed-in `_ACCOUNT` config dir). It saves `qr.png`/`dialog.png`; decode the QR too.
- Check Cloud's Remote control page (Sessions and Hosts tabs) and the shared chat at 375 px wide: chat-only connectors appear only under Sessions, never as computer cards under Hosts.

## Your computers in Desktop

Desktop lists the account's hosts (Settings → Servers and the server switcher, **Your computers**) and opens them through `/api/v1/remote-hosts/{id}/runtime`. Unit coverage: `cargo test -p haarness-cli --bin haarness desktop_lists_other_computers desktop_relay_sign_in`, `npm --prefix apps/desktop test` (`tests/remote-hosts.test.ts`, `tests/servers.test.ts`). `npx playwright test e2e/remote-hosts.spec.ts` plays Cloud itself in front of a real `haarness serve` gateway (no Cloud needed): listing, opening, stream cuts across sign-in renewals and a project server restart, sign-out and account switch, revoked computers, and a Cloud session's ticket on its event stream. Against a local Cloud with the fixture identity provider, `HAARNESS_REMOTE_E2E=1 HAARNESS_CLOUD_API_URL=http://127.0.0.1:19080 scripts/test-remote-hosts-live.py [state]` signs a fixture user in with the device flow, shares three computers (one away, one being Desktop's own), runs the spec's live case (chat, terminal and folder browser through the relay) and revokes everything; `HAARNESS_REMOTE_HOSTS_LONG=1` also waits past the ten minutes a relayed sign-in lives.

## Complete the user journey

Follow the [test plan](references/remote-session-test-plan.md) for full-host and published-chat Chrome control, real-model file reading, bidirectional tmux chat updates, permissions/questions, reconnect with unsent draft retention, cross-replica routing and independent credential revocation. A connected badge or successful click alone is not a pass: verify fresh runtime effects and authoritative history after recovery.

Use `remote run --chat THREAD_ID` to publish. The global `--session` flag is an alias for native thread selection, not publication. Background `remote enable` does not install boot/login startup integration.

Stop sharing/revoke/disable only owned fixtures and close their streams, PTYs and tmux sessions. Project close, unpublish and host revocation must retain native files/history; never apply rented-session deletion to remote hosts. Report exact revisions, automated/Chrome/tmux/provider evidence separately, cleanup status and any untested boundary. Leave a demo running only when needed for user testing.

## Explore beyond this guide

Treat the documented flows as a starting point. Do not hesitate to test relevant behavior that is not documented here: inspect the current implementation, follow unexpected results, and cover interactions and failure paths suggested by the task. Stay within the authorized environment and scope. Fix discovered bugs when authorized, verify the fixes, and record newly validated steps and remaining gaps so this skill improves with use.
