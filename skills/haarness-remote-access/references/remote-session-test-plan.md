# Remote-session test plan

Use with [the remote-access skill](../SKILL.md). Resolve IDs and addresses from the current fixture; do not reuse historical demo IDs or cached credentials. Choose checks for the changed surface; a full end-to-end claim needs automated relay, browser, native TUI and real-provider evidence.

## Contents

- [Register and discover](#1-register-and-discover)
- [SDKs and real files](#2-use-the-sdk-adapters-and-real-files)
- [Chrome and tmux](#3-prove-chrome-and-tmux-control-the-same-chat)
- [Publish one chat tree](#4-publish-one-chat-tree)
- [Interrupt and reconcile](#5-interrupt-transport-and-reconcile)
- [Authorization, routing and load](#6-authorization-routing-and-bounded-load)
- [Cleanup and evidence](#7-cleanup-and-evidence)
- [Troubleshooting](#diagnose-before-repeating)

## 1. Register and discover

With an isolated workspace/data/config environment, run the selected CLI:

```sh
"$haarness_bin" --workspace "$fixture_workspace" remote run --name "$fixture_name"
# Inspect from another terminal with the same isolated configuration:
"$haarness_bin" remote status
"$haarness_bin" remote hosts
"$haarness_bin" remote chats
```

Use `remote enable --name NAME` instead of `run` for background lifecycle testing. Do not launch duplicate connectors for one identity. `status` proves local process liveness; Cloud `connected` proves its relay lease. Read a project and complete a turn to prove the runtime works.

Record the registered host ID, then resolve the project whose path equals the fixture workspace through `host_projects()` / `hostProjects()`. Create two native sibling chats A/B and a child of A. Record their thread IDs. The existing [live fixture](../../../scripts/test-remote-live.py) demonstrates native daemon discovery through the `desktop` JSON bridge; its URL/token output is private. Refresh discovery after replacing a daemon.

In Chrome **Remote hosts**, verify live appearance/status, rename, hidden-folder browsing, open an existing project, and select its chats. Opening a project must retain its existing files/history. Closing its advertisement must remove it from discovery without deleting it; reopening restores the same project identity.

## 2. Use the SDK adapters and real files

| Runtime target | Python sync / async | TypeScript |
| --- | --- | --- |
| Native host project | `cloud.remote_host(host_id).workspace(project_id)` | `cloud.remoteHost(hostId).workspace(projectId)` |
| Published native chat tree | `cloud.remote_session(publication_id)` | `cloud.remoteSession(publicationId)` |
| Rented Cloud workspace | `cloud.workspace(workspace_id)` | `cloud.workspace(workspaceId)` |

Keep host, project, publication and thread IDs distinct. Runtime chat methods still require native thread IDs; a publication's `threadId` identifies its root. Use the native connector for `/connect`; generated JSON HTTP methods do not implement its WebSocket protocol.

Through each applicable adapter, test history/turns, files and event cancellation; full-host mode also supports PTYs and host project operations. Python sync/async and TypeScript must preserve dynamic Cloud auth on nested HTTP, SSE and PTY clients. A rejected Cloud key must not fall back to saved OAuth/local server auth. Close clients and iterators so capacity is released.

Follow the [files skill](../../haarness-files-uploads/SKILL.md): show `.haarness`, `.git` and nested dotfiles, compare uploaded/downloaded binary bytes, and verify an SDK-created file appears in Chrome without a reload. Publication events must carry shared `files.updated`/`assets.updated` invalidations while excluding unrelated chat data.

For real-provider proof, upload a file containing a fresh marker known only from its contents. Ask the model to read it, then inspect the actual file-read tool result and completed turn. Ask chat B to read the same file: the project filesystem is shared, histories are separate. Use the host's configured provider; the Cloud control key alone does not grant model access. Billing assertions, when in scope, use the [native billing/router skill](../../haarness-native-billing-router/SKILL.md).

Test the physical Chrome file picker separately. A blocked extension file-URL permission is a browser test limitation, not an API upload failure or a passed picker test. Respect browser-tool permissions and record the boundary. At narrow widths, close a file overlay before sending; confirm the user message entered history and a turn started instead of treating a successful click as proof.

## 3. Prove Chrome and tmux control the same chat

Create a disposable tmux session and use the same fixture environment. Set `HAARNESS_SERVER_TOKEN` privately for the currently discovered local runtime; never put it in command arguments, screenshots or captured logs. In the pane:

```sh
"$haarness_bin" --workspace "$fixture_workspace" --thread "$thread_id" attach "$runtime_url"
```

Send a fresh marker prompt through Chrome. Verify the same thread receives it in tmux, streams its response and settles to the correct turn state. Send a different prompt from the TUI using `tmux send-keys`; verify Chrome receives the new user message and response without navigating/reloading. Capture the visible pane and final API history as evidence, with credentials omitted.

Exercise stop/cancel and an operation configured to require approval. Approve through the other client and assert one execution; deny another and assert no side effect. If questions are in scope, answer through the other client and verify the paused turn resumes once. Existing grants may suppress prompts, so configure only the disposable fixture to require them. Do not blanket-approve a user's workspace to make a test pass.

## 4. Publish one chat tree

Use the CLI's separate publication flag:

```sh
"$haarness_bin" --workspace "$fixture_workspace" remote run --chat "$thread_id" --name "$publication_name"
```

Or publish through the registered host using `projectId` and `threadId`. Record the returned publication ID and use that for the remote-session adapter/web route. Test a session-only connector as well as a publication on a full host; the former must not accept a forged sibling publication.

- Published root A and its child can list/read/respond; sibling B's history, messages and events are excluded.
- Direct API calls to create/fork chats, access host controls, PTYs or server shutdown are denied, even if the UI hides them.
- Chrome disables new-chat creation and omits terminal/host-wide capability controls; allowed Files/Changes/Tasks remain usable.
- Shared project files remain accessible and live updates arrive. Sharing one chat must not imply private per-chat files.
- Unpublish closes access. Reconnecting an old connector must not revive the revoked publication. Local chats/files remain intact.

## 5. Interrupt transport and reconcile

Keep an unsent Chrome draft throughout recovery tests. Do not manually refresh to hide a failed event reconnection. Use owned fixture PIDs/IDs and stop only one failure source at a time.

| Failure | Required proof after recovery |
| --- | --- |
| Connector stopped/restarted | Offline/reconnecting state, same host identity, draft/history retained, fresh bidirectional TUI/Chrome turn |
| Local daemon replaced | Connector discovers the new runtime address/token; same project/chat/files work; stale cached attachment is not reused |
| Sandbox API restarted | Browser recovers even if its proxy leaves the old SSE body open; a fresh TUI reply appears without reload |
| Accepted write/turn loses response | Reconcile authoritative turn/history/file state; one side effect, no automatic write replay |
| Project close/reopen | Advertisement changes live, native work/files/history survive, project ID remains stable |
| Consumer abandons SSE/PTY | Stream slots are released and later requests succeed |

Current timing: host heartbeat 5 s, stale lease 20 s plus reaper cadence; native stream idle timeout 40 s; browser event watchdog 45 s. Inspect current constants if timing code changed. Allow recovery deadlines beyond these thresholds; short sleeps or an unchanged green badge are insufficient. The live fixture idles a PTY over 40 s, then sends new input to prove keepalive.

For ambiguous mutations use native request IDs where supported and read authoritative state before retrying. The relay must not replay writes. The existing live script proves this with approved local work that completes while transport is disconnected.

## 6. Authorization, routing and bounded load

Run Cloud `cargo test -p cloud-api --test remote` with a real test database configured. Its three cases cover registry scopes/ownership/revocation, real multiplexed relay across two API replicas with live key revocation, and pagination/replacement lease fencing. Confirm cases executed instead of skipping.

- `remote:read`: account discovery/entity events only. `remote:connect`: owned registration/connector/publication. `remote:control`: runtime, rename/revoke/unpublish. Role checks are independent: viewer discovery, builder-or-higher control; only the registering user connects/publishes.
- Use different consumer/publisher credentials. Revoking a consumer closes its streams without dropping another publisher; revoking publisher access disconnects that host. Check fresh requests and existing streams, including expiry/permission changes when auth is affected. Access revalidation runs every 5 s.
- Wrong-account IDs return 404; conversation-scope denials 403; offline/stale generation 409; exhausted relay capacity 429. Cookie mutations require CSRF and browser WebSockets the allowed Origin. Native connectors use bearer auth.
- Multi-replica tests require each `HAARNESS_CLOUD_RELAY_ORIGIN` to identify that API process directly, never the shared load balancer. Route a consumer to the non-owning replica and verify HTTP, events and terminal traffic; forwarded auth must be revalidated and stale generations fenced.
- Current limits: 32 streams per host, 64 per API process, eight 32 KiB response-credit chunks, 16 MiB request bodies and 24 MiB encoded frames. Use the fixture's 10 MiB binary round trip, 200 reads at concurrency 20, and 32 held streams/429/release sequence. Coordinate load with other sandbox users; do not remove unrelated sessions to create capacity.

## 7. Cleanup and evidence

Close owned SDK streams/clients and PTYs, stop the fixture TUI/tmux session, unpublish test chats and revoke owned hosts. Exercise `remote disable HOST_ID`, then `remote forget HOST_ID` for a stopped identity; forget revokes before removing its local identity. A later explicit enable creates a fresh host ID. Verify old IDs remain inaccessible and original local files/history remain.

Stop only the fixture daemon after connector/terminal cleanup. Keep its directory until owned processes stop and evidence is saved. Do not use rented-workspace worker cleanup, delete user projects, kill all Haarness processes or remove another host's registration. If a manual demo remains running at the user's request, report its URL, lifecycle and how to stop it, without secrets.

Report source SHAs/binary, fixture/provider mode, executed checks and counts, Chrome/tmux observed effects, stress/recovery results, cleanup IDs/status and limitations. Automated echo, real-provider turns, browser picker, SDKs and production deployment are separate verdicts.

## Diagnose before repeating

| Symptom | First checks |
| --- | --- |
| Cloud says connected, runtime calls fail | Local daemon health/capability, current endpoint/token, connector logs; status/lease does not prove daemon health |
| TUI attach refuses connection after restart | Rediscover the fixture runtime; old URL/token are stale |
| HTTP works but terminal does not | `/api` WebSocket upgrades in the dev/ingress proxy, allowed Origin and full-host scope |
| Chat stays stale after API restart | Event heartbeat/watchdog recovery and a fresh cross-client message; do not mask with reload |
| Scoped chat files do not update | Shared filesystem event allowlist as well as conversation filtering |
| 403/404/409 after revoke | Capability/account/scope and durable revoked identity; explicit fresh registration, never auto-resurrection |
| Schema/SDK checks unexpectedly regress | Matching Cloud checkout supplied through `HAARNESS_CLOUD_REPO`; do not generate from a stale sibling checkout |
