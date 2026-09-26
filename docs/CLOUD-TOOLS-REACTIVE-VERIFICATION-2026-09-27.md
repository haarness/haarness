# Reactive Cloud tools verification — 2026-09-27

Native base: `fb09edfe75d8346f8297c4742396dd22ee2febf2`.
Cloud companion: `e144b8906bec733776c9632825847146b9cfd893`, built and running in the local
Docker stack at `http://localhost:18000`. Immutable local image:
`sha256:8b7fbab3503216f9b5cedfdc6767ae9c2a4b420527ea2d14f3499b19bf32b719`.
Remote sessions ran on Fly.io with real Cloud model calls. No production deployment was performed.

## Behavior and architecture

`cloud_agent_manage` keeps authoritative HTTP commands and queries. The long waits in
`cloud_agent_session` now use push notifications: the account WebSocket for provisioning,
and the existing authenticated runtime SSE stream for turn state and permission changes.
This follows Upstream's subscribe/snapshot/recovery pattern without replacing Haarness's
already-streaming runtime protocol. Token deltas do not cause extra state reads.

Subscriptions start before snapshots, coalesce notifications while HTTP is in flight,
resync on connection/gap events, and filter by account/session/workspace or thread/turn.
Healthy connections perform a defensive reconciliation every 60 seconds; disconnected
or older servers retain the prior one-second polling fallback. Connections have bounded
handshakes, idle watchdogs, message sizes, retry backoff and jitter. OAuth rejection
refreshes the token; runtime reconnects renew attach tickets. Runtime-generation errors
reattach and retry reads only. Submitted messages are never replayed by reconnect recovery.
Cancellation and dropped tool futures dispose of the subscription. An observational wait
never interrupts remote work; a synchronous ask keeps its existing best-effort interrupt.

Native OAuth WebSockets can omit Origin. Cookie authentication still requires the exact
configured browser Origin, and a present foreign Origin is rejected for every auth method.
Normal scope, account-membership, expiry and revocation checks remain in force.

The LLM-facing schema explicitly supports both independent sessions with inline configuration
and optional reusable agent templates. `ask` can create/start/message in one call without
`sessionId` or `cloudAgentId`. Session names use `title`; `name` belongs to `save_agent`.
Native shell tools need no invented MCP server; shell policy is `permissions.bash`.

## Automated checks

- `cargo test -p haarness-account`: **34 passed** (10 unit, 20 Cloud HTTP/WS/SSE integration,
  4 account-session tests). Coverage includes no healthy one-second polling, unrelated-event
  and token-delta filtering, sub-700 ms event completion, child approvals, reconnect/resync,
  rejected OAuth refresh, ticket renewal, runtime generation changes, fallback, timeout,
  cancellation, subscription cleanup, and freestyle/template asks with inline overrides.
- `cargo test -p haarness-core cloud`: **3 passed** for catalog discovery and action/target/plan permissions.
- `cargo test -p haarness-cli account::`: **3 passed**.
- Focused server schema test: **1 passed**, including optional-template launch validation.
- Strict account Clippy, incremental CLI build, and diff whitespace checks passed.
- Cloud API real-Postgres WebSocket integration: **1 passed**, covering native no-Origin OAuth,
  missing-Origin cookies, mixed credentials, foreign origins/accounts, commit/rollback,
  secret-free events, revocation and reconnect. Strict Cloud API Clippy passed.

## Real LLM, Fly and tmux proof

The repeatable opt-in script is `scripts/test-cloud-tools-live.py`. Both the actual orchestrator
and the remote runtime used `openai/gpt-5-mini` in the final passing run. Launch prompts are
ordinary English; no forced tool JSON is supplied to those model turns. Deterministic echo
turns exercise the same registered tools for follow-up lifecycle and cancellation assertions.

| Natural-language request | Observed model calls and result |
| --- | --- |
| Start a fresh Cloud session without a saved agent; run `uname -s` | One `cloud_agent_session.ask` with title and inline model/Bash permissions; no agent ID. Returned `FREESTYLE_PUSH_OK Linux`. |
| Find the named saved Cloud agent and launch a session from it | `cloud_agent_manage.list`, then session `create` with the returned template ID, then `ask` with the created session ID. Returned `TEMPLATE_PUSH_OK`. |
| Continue in that same Cloud session | One `ask` using the same session ID; no new launch. Returned `SAME_SESSION_PUSH_OK`. |
| I don't have an agent template; ask a new Cloud session | One `ask` with title and inline configuration; no agent ID. Returned `NO_TEMPLATE_NEEDED_OK`. |

The first freestyle session was also saved as a reusable template and discovered by name.
The script checked its title and independent ownership, used actual tmux input to invoke
`messages`, and asserted that the remote reply appeared in the rendered TUI. A one-second
ask timeout was resumed with the original turn ID. Cancelling a separate observational wait
left its remote turn running; an explicit abort then interrupted that exact turn. Debug logs
confirmed three actual account WebSocket subscriptions and the runtime SSE subscriptions.

Final run resources, all deleted with successful cleanup operations and authenticated 404 readback:

- Freestyle session: `01a0de6c-2623-70d6-9bb6-21f98636982b`.
- Template session: `01a0de6c-e859-7742-bbf5-67f04fb94c43`.
- No-template wording session: `01a0de6d-8738-765e-88fe-023774d1362f`.
- Temporary template: `01a0de6c-aca6-77bb-b1dc-2ff8a8df3a22`.

Local evidence is under `target/cloud-tools-live/final/`: actual tool-call JSON for each
wording, result snapshots, `tmux-messages.txt`, connection logs and cleanup-operation results.
The test's daemon, tmux session and temporary files were removed. Existing user sessions,
Chrome, frontend, API and other tmux sessions were preserved.

An initial live run exposed ambiguous schema wording (`name` versus `title`, unnecessary shell
MCP configuration); the descriptions were clarified and the final model selected the intended
single-call freestyle path. Its Nano runtime also repeated a shell fixture command until the
bounded wait returned `running`; the test session was deleted. The final suite uses Mini and
an explicit single-command fixture. These tests validate the observed prompts and recovery
paths, not a guarantee that every model always follows instructions.

To repeat against a signed-in development Cloud account:

```sh
HAARNESS_CLOUD_TOOLS_LIVE=1 \
HAARNESS_CLOUD_API_URL=http://localhost:18000 \
HAARNESS_CLOUD_CONFIG_DIR=/path/to/existing/account-profile \
python3 scripts/test-cloud-tools-live.py
```

The script discovers the requested remote model, creates only its own disposable resources,
and verifies deletion before exiting. It never records credentials in its evidence.
