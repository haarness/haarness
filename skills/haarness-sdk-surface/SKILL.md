---
name: haarness-sdk-surface
description: Verify Haarness Python sync/async and TypeScript SDK coverage of runtime and Cloud APIs, including remote hosts/published chats, billing, rented workspaces, schedules, files, marketplace and WebSocket events.
---

# SDK contract and end-to-end coverage

The native repository contains `sdks/`; Cloud contracts generate `haarness-cloud/docs/openapi.json`. Read `haarness/docs/records/2026-09-27-cloud-sdk-verification.md` and the native build skill. Regenerate from the matching Cloud checkout, not a stale sibling primary checkout:

```sh
# Cloud checkout
scripts/openapi.sh --update
# Native checkout
cp "$cloud_repo/docs/openapi.json" sdks/cloud-openapi.json
HAARNESS_CLOUD_REPO="$cloud_repo" python3 sdks/generate.py
HAARNESS_CLOUD_REPO="$cloud_repo" python3 sdks/generate.py --check
npm --prefix sdks/typescript run build
npm --prefix sdks/typescript test
sdks/python/.venv/bin/python -m pytest sdks/python/tests -q
```

Use the smallest relevant tests during iteration. `scripts/test-sdks.sh` is the full packaging/fresh-consumer gate; inspect it first because it can install dependencies. Do not reinstall node_modules while another test uses them.

The SDK must expose every documented Cloud HTTP operation and WebSocket stream, and every server runtime operation. Test Python sync and async and TypeScript with real account auth/refresh. Check method/path/query/body encoding, binary uploads/downloads, errors, cancellation, pagination and redirect policy. Request options must remain compatible when adding account-scoped queries.

Run `sdks/tests/cloud_e2e.py` and `cloud_ts_e2e.mjs` against a disposable Cloud fixture stack, and native server lifecycle/file tests. Read their flags and `cloud_fixture.py` authentication helper. Explicit consent may be required when fixture auto-consent is disabled; do not bypass OAuth on a real account.

Cover independent workspace launch, agent-template launch, multiple chats, shared files, turn streams, Cloud schedules, marketplace installation and cleanup. Billing checks include accountId on reads and mutations, wallet/holds, plans, checkout, portal, cancel/resume, payments, usage and ledger. Verify account membership and least-privileged tokens; a billing-only event token sees billing changes and no workspace data.

For live streams, test ready/resync, correct account filtering, updates during a snapshot fetch, disconnect/reconnect, refreshed credentials and stopping the iterator. Assert state converges from authoritative HTTP snapshots without duplicate monetary effects.

Capture exact source SHAs, generated-contract checks, test results, real provider/fixture distinction and cleanup outcomes. Existing verification reports are references, not current pass results.

## Remote host and published-chat adapters

Follow the [remote-access skill](../haarness-remote-access/SKILL.md) and its live connector test. Python sync/async use `cloud.remote_host(host_id).workspace(project_id)` or `cloud.remote_session(publication_id)`; TypeScript uses `cloud.remoteHost(hostId).workspace(projectId)` or `cloud.remoteSession(publicationId)`. Rented compute uses the separate `cloud.workspace(workspace_id)` adapter (`cloud.workspace(workspaceId)` in TypeScript); `new_workspace`/`newWorkspace` creates one and returns that handle. Resolve project IDs from the host catalog; publication records contain the native `threadId` used for chat operations.

Test both registry APIs and nested runtime HTTP, SSE and PTY behavior, including dynamic credential refresh/revocation, cancellation, binary files, reconnect and account boundaries. Published adapters allow the selected chat tree and shared project files while denying siblings and host/terminal controls. The native CLI implements the low-level connector WebSocket protocol; a generated JSON method for its upgrade endpoint is not connector coverage. `scripts/test-remote-live.py` orchestrates Python sync/async plus `sdks/tests/remote_e2e.mjs` and owns fixture cleanup. Browser/real-model proof is separate.

## Unified API keys

`HAARNESS_API_KEY` authenticates Cloud API requests and model inference according to the key's capabilities. An explicit SDK token takes precedence; the environment key takes precedence over the older `HAARNESS_CLOUD_TOKEN`. It must never fall back to OAuth or `HAARNESS_SERVER_TOKEN` after rejection.

Run the opt-in `sdks/tests/api_keys_e2e.py` with the SDK Python environment, a freshly built router/API, `HAARNESS_KEY_E2E=1`, the loopback `HAARNESS_CLOUD_API_URL`, and `HAARNESS_E2E_OAUTH_FILE` pointing to private fixture OAuth JSON. It invokes `api_keys_ts_e2e.mjs` too. It creates/revokes its own keys and deletes its workspace. It covers real inference, model allowlists, sync/async chats, shared file uploads, Cloud schedules, denied payment/key administration, permission edits, old attach-ticket invalidation, rotation without budget reset, and actual expiry. Do not print the credential JSON.

In the Cloud checkout, `scripts/test-api-keys-cli-live.py` uses the same opt-in, API URL and fresh fixture OAuth file plus `HAARNESS_CLOUD_BIN`. It exercises create/list/edit/rotate/revoke through the CLI and proves that denied or revoked API keys do not fall back to saved OAuth. Credits serialize as decimal strings; compare their numeric decimal value, not a fixed number of trailing zeroes.

For Chrome, check the single API keys entry, edit form preservation during WebSocket updates, expiry, inactive rows and the legacy `/app/router/keys` redirect. Follow the applicable browser permission policy for credential mutations; backend tests and Chrome observation are distinct evidence. Key model caps apply to direct calls through that key, while workspace compute and scheduled agent work use account/runtime limits.

## Explore beyond this guide

Treat the documented flows as a starting point. Do not hesitate to test relevant behavior that is not documented here: inspect the current implementation, follow unexpected results, and cover interactions and failure paths suggested by the task. Stay within the authorized environment and scope. Fix discovered bugs when authorized, verify the fixes, and record newly validated steps and remaining gaps so this skill improves with use.

## Release, browser-host and router additions

Run `sdks/tests/releases_e2e.mjs` and `releases_e2e.py` against an owned local Cloud
stack with `RELEASE_TEST_URL` and an environment-only `HAARNESS_RELEASE_TOKEN`.
These publish a disposable preview, verify binary/text downloads, withdraw it and
restore the prior publication in `finally`. Never point this mutation test at production.
See [release testing](../haarness-release/SKILL.md) for real packages and quarantine.

The native scenario now exercises 128 non-account operations in each of TypeScript,
Python sync and Python async, including actual browser-host WebSockets, TCP tunnels,
draft adoption, lease rejection and shared HTML. Four native account adapters still
need the authenticated Cloud tool suite. New route modules and unmatched Cloud route
registrations must fail coverage checks, rather than relying on a stale route count.

For real inference, run `router_inference.mjs` and `router_inference.py` with an
expiring disposable API key (models:read, models:invoke; routers:write for discovery),
`SDK_ROUTER_URL`, `SDK_ROUTER_ID`, `SDK_NAMED_ROUTER_URL`, and `SDK_CLOUD_URL`.
Test the default paid router and a named BYOK router; compare billing readbacks and
revoke the key in cleanup. These tests call a real model and consume credits on the
default route. `relay_socket.mjs` / `.py` take `SDK_CLOUD_URL` and `SDK_CLOUD_TOKEN`,
register disposable hosts, exchange JSON frames, confirm connected state and revoke.
