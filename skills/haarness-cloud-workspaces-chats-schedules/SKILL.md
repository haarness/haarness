---
name: haarness-cloud-workspaces-chats-schedules
description: Verify Haarness Cloud agents, independent workspaces, multiple chats and Cloud schedules through API, SDK, LLM tools and Chrome, including cleanup and reconnection.
---

# Workspaces, chats and Cloud schedules

Read `haarness-cloud/docs/CLOUD-SCHEDULES.md`, `haarness/docs/records/2026-09-27-cloud-sdk-verification.md` and the native build skill. Use disposable names/IDs and keep a cleanup manifest. A rented Cloud workspace owns the machine and filesystem. Chats own conversation histories and turns inside that workspace. An agent is an optional reusable template. Local schedules and Cloud worker schedules are separate products.

For user-owned machines, remote hosts or published native chats, use the [remote-access skill](../haarness-remote-access/SKILL.md). A shared chat (a remote-session record in the API) is a sharing record for a native chat tree, not rented compute. Its publication ID differs from its native thread ID. Remote project close/unpublish/revoke retains local files and history; the worker/storage deletion procedure below applies only to rented test workspaces. Native schedules reached through a remote runtime remain native schedules.

## API/SDK lifecycle

Create a workspace with no cloudAgentId and another from a listed template. Confirm template snapshots and overrides, start/attach, stop/restart, independent chats, separate turns/histories and shared workspace files. Use explicit workspaceId/chatId/turnId; don't silently choose an arbitrary chat in a workspace. Assert account isolation, expected version conflicts and idempotent retries. Every `{id}` route also accepts the id a workspace had as a Cloud session before the rename; cover one old id.

Exercise `haarness-cloud/scripts/test-cloud-parity.py`, the native `sdks/tests/cloud_e2e.py` and `sdks/tests/cloud_ts_e2e.mjs` after reading their flags/environment. Use actual local runtime or approved test provider; label which was used. `scripts/e2e.sh` runs the baseline nine-step fixture journey with cleanup.

## Ask the LLM and inspect real actions

From `haarness/`, read and run `scripts/test-cloud-tools-live.py` with its explicit opt-in, existing isolated login, available cheap model and evidence directory. It uses real Cloud-backed orchestration plus deterministic follow-up fixtures. Ask in varied wording:

- Start a fresh Cloud workspace without a template.
- List my Cloud agents and launch one from the named template.
- Create another chat in the same workspace, write a marker file in chat A and read it in chat B.
- Schedule a future Cloud task; list it, update it, disable/re-enable it, and remove it.

Check actual tool calls/results and persisted IDs, not just an assistant claim. Prove the scheduled run creates/uses the intended chat and sends a real runtime turn. Cover one-shot, recurring, overlap, paused/deleted targets, retry, cancellation, and revision conflicts. Cloud schedules must run while the local TUI/Desktop is stopped; local schedules must not appear as Cloud schedules.

## Chrome and cleanup

Open workspaces/agents/schedules in Chrome control. Watch transitions, chat creation/switching, streaming, live list changes, disconnect/reconnect and attach recovery. Preserve editor/composer state across WebSocket invalidations; no page reload or flickering empty list. Verify pane navigation and outside-click dismissal.

Delete only owned schedules/templates/workspaces. Wait for worker reconciliation, runtime shutdown, gateway ticket rejection, chat/runtime storage cleanup and intended retained schedule history. Retain failed resource IDs if cleanup fails. Don't delete an unrelated running workspace to satisfy limits. Test both independent and template-created workspace cleanup.

## Explore beyond this guide

Treat the documented flows as a starting point. Do not hesitate to test relevant behavior that is not documented here: inspect the current implementation, follow unexpected results, and cover interactions and failure paths suggested by the task. Stay within the authorized environment and scope. Fix discovered bugs when authorized, verify the fixes, and record newly validated steps and remaining gaps so this skill improves with use.
