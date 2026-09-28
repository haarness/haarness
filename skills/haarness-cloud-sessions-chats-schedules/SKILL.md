---
name: haarness-cloud-sessions-chats-schedules
description: Verify Haarness Cloud agents, independent sessions, multiple chats and Cloud schedules through API, SDK, LLM tools and Chrome, including cleanup and reconnection.
---

# Sessions, chats and Cloud schedules

Read `haarness-cloud/docs/CLOUD-SCHEDULES.md`, `haarness/docs/CLOUD-SDK-VERIFICATION-2026-09-27.md` and the native build skill. Use disposable names/IDs and keep a cleanup manifest. A rented Cloud session owns the machine, workspace and filesystem. Chats own conversation histories and turns inside that session. An agent is an optional reusable template. Local schedules and Cloud worker schedules are separate products.

For user-owned machines, remote hosts or published native chats, use the [remote-access skill](../haarness-remote-access/SKILL.md). A published remote session is a sharing record for a native chat tree, not rented compute. Its publication ID differs from its native thread ID. Remote project close/unpublish/revoke retains local files and history; the worker/storage deletion procedure below applies only to rented test sessions. Native schedules reached through a remote runtime remain native schedules.

## API/SDK lifecycle

Create a session with no cloudAgentId and another from a listed template. Confirm template snapshots and overrides, start/attach, stop/restart, independent chats, separate turns/histories and shared workspace files. Use explicit sessionId/chatId/turnId; don't silently choose an arbitrary chat in a session. Assert account isolation, expected version conflicts and idempotent retries.

Exercise `haarness-cloud/scripts/test-cloud-parity.py`, the native `sdks/tests/cloud_e2e.py` and `sdks/tests/cloud_ts_e2e.mjs` after reading their flags/environment. Use actual local runtime or approved test provider; label which was used. `scripts/e2e.sh` runs the baseline nine-step fixture journey with cleanup.

## Ask the LLM and inspect real actions

From `haarness/`, read and run `scripts/test-cloud-tools-live.py` with its explicit opt-in, existing isolated login, available cheap model and evidence directory. It uses real Cloud-backed orchestration plus deterministic follow-up fixtures. Ask in varied wording:

- Start a fresh Cloud session without a template.
- List my Cloud agents and launch one from the named template.
- Create another chat in the same session, write a marker file in chat A and read it in chat B.
- Schedule a future Cloud task; list it, update it, disable/re-enable it, and remove it.

Check actual tool calls/results and persisted IDs, not just an assistant claim. Prove the scheduled run creates/uses the intended chat and sends a real runtime turn. Cover one-shot, recurring, overlap, paused/deleted targets, retry, cancellation, and revision conflicts. Cloud schedules must run while the local TUI/Desktop is stopped; local schedules must not appear as Cloud schedules.

## Chrome and cleanup

Open sessions/agents/schedules in Chrome control. Watch transitions, chat creation/switching, streaming, live list changes, disconnect/reconnect and attach recovery. Preserve editor/composer state across WebSocket invalidations; no page reload or flickering empty list. Verify pane navigation and outside-click dismissal.

Delete only owned schedules/templates/sessions. Wait for worker reconciliation, runtime shutdown, gateway ticket rejection, chat/runtime storage cleanup and intended retained schedule history. Retain failed resource IDs if cleanup fails. Don't delete an unrelated running session to satisfy limits. Test both independent and template-created session cleanup.
