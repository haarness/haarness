# Cloud sessions, chats, schedules and SDK verification — 2026-09-27

Implementation bases: native `a33337eb9ec8539653d13128e853cab1856955af`,
Cloud `e144b8906bec733776c9632825847146b9cfd893`.

## Scope and changes

- Explicit Cloud session (machine/files/configuration), chat (root thread/history)
  and turn IDs across native account tools, native HTTP adapters and SDK handles.
  Legacy single-chat calls remain compatible; ambiguous multi-chat calls fail
  instead of silently selecting a conversation. Templates remain optional.
- Durable account-owned Cloud schedules, independent of device-local schedules:
  versioned CRUD, pause/resume, run-now, history, account WebSocket invalidations,
  stopped-session wakeup, stable turn request IDs and normal permission handling.
- Generated TypeScript and Python sync/async coverage of 116 registered native
  operations and 126 documented Cloud operations (125 HTTP plus WebSocket).
  Runtime attachment tickets and account/router credentials remain separate.
- Fixed TypeScript duplicate Authorization headers and stale-generation envelope
  handling. Python SSE now renews a rejected attachment once while preserving its
  cursor, signals resync on generation change, and bounds failed renewal. Concurrent
  Python chat connections share one runtime client.
- File API mutations emit `files.updated` only after success. The Cloud explorer
  now recognizes the actual `turn.finished` event, including other chats in the
  shared session. Refresh preserves the mounted composer and draft.
- Session deletion cancels pending schedules and prevents late worker responses
  from reviving them. Retention preserves active work and purges terminal history
  after 30 days. Existing idle detection accounts for activity across all chats.

## Automated evidence

| Surface | Result |
| --- | --- |
| Native protocol/server | 261 tests passed |
| Native account client | 37 tests passed |
| Cloud tool plan-mode permissions | Mutation ceiling regression passed, including remembered approval |
| Native SDK runtime journey | 112/112 non-account operations in each of TypeScript, Python sync and async |
| Native Cloud HTTP adapters | All four exercised against the live account in the model/tool scenario |
| SDK wire/transport tests | 12 TypeScript and 22 Python tests passed; every one of 125 Cloud HTTP operations covered per client |
| SDK generation/types | Live schema check, source-generated drift check, TS build, Python mypy and Desktop consumer typecheck passed |
| Packaging | npm tarball and Python wheel installed in clean consumers; native operation journey and consumer typing passed, including Python 3.10 |
| Final Python wheel | Rebuilt after connection coalescing fix and checked byte-for-byte against all SDK Python sources |
| File API | UTF-8/base64 round-trips, stale-write preservation, invalid payload/path rejection, and successful-write event ordering passed |
| Cloud web working tree | 136 tests passed before the new refresh test; all six chat tests including the refresh regression passed afterward |
| Exact staged Cloud web | 127 tests passed across 31 files, typecheck and production build/prerender passed |
| Cloud API/CLI | 133 tests passed |
| Additional Cloud packages | 462 cases exercised across contracts/domain/E2E/gateway/integrations/router/store/worker; failures resolved as described below |
| Cloud lint | API/worker all-target clippy with warnings denied passed |
| Billing journey | All nine isolated end-to-end steps passed in 133.28 seconds |

The broad Cloud run found outdated fixture inventories for the added periodic job;
those inventories were fixed without removing coverage. It also exceeded the small
PostgreSQL fixture's lock budget in concurrent billing, job-store and lifecycle
suites, and one router settlement assertion timed out under concurrent load.
The complete affected suites passed serially: router 14, billing 19, job-store 14,
worker unit 20 and lifecycle 25. No production check, assertion or test timeout was
relaxed. One S3/R2 integration test remains explicitly ignored without a configured
bucket. Use `-- --test-threads=1` on small shared PostgreSQL fixtures.

The packaging gate preceded the final Python connection-coalescing regression;
the updated source passed its 22-test suite and mypy, then its rebuilt wheel was
checked against the source. Generated contracts are derived from current OpenAPI;
JSON-valued configuration fields remain JSON-valued rather than invented types.

## Live Cloud SDK journeys

TypeScript, Python sync and Python async each exercised the real account API,
worker, gateway and native runtime on an isolated fixture stack. Each covered:
freestyle session creation, two separate chats, rename, shared text/binary uploads,
stale-write 409, file rename/delete, chat history isolation and cross-chat turn
rejection. All seven Cloud schedule operations were exercised, including version
conflicts, idempotent run-now, timed execution, execution while the client was
closed, waking a stopped session, preserved chats/files and archived-target safety.
The TypeScript journey observed schedule/run WebSocket notifications and reused
one runtime handle through a generation change. Test session deletion waited for
the cleanup operation to succeed.

A fresh fixture Pro purchase additionally checked that subscription and entitlement
plan codes match, with 10,000 AI credits and 50 cloud units. The existing development
account has an older explicit `source_kind=test` Max allowance layered over a Pro
fixture subscription; its displayed larger balance was traced to that test grant,
not a new payment or a pricing calculation. Existing account data was preserved.

Billing evidence covers fixture checkout/webhook replay, duplicate credit-grant
prevention, streaming and non-streaming router usage, exact AI and compute wallet
identities, reservation release, idle/restart, exhaustion, top-up, overage and
volume cleanup. No real card was charged.

## Real model, tmux and Chrome

The opt-in `scripts/test-cloud-tools-live.py` passed model discovery, freestyle
launch, actual remote reply, tmux tool messages, timeout-then-wait without duplicate
submission, observer cancellation/explicit abort, separate chat creation on the
same machine, natural-language selection of `cloud_schedule`, a real timed reply,
template discovery/launch, contextual follow-up and a differently phrased request
without a template. It observed real account WebSocket and runtime event streams.
All sessions created by that script were deleted through completed operations.

Chrome at `http://localhost:18173` verified:

1. Create and edit a file, download identical bytes, and rename it.
2. Ask the real model to read it; a second chat read the same file independently.
3. Ask the agent to create a file; it appeared in the explorer automatically.
4. Upload UTF-8 text and binary bytes through the authenticated SDK/API and verify
   byte-for-byte readback; ask the model to read the uploaded text and recover
   `Color: amber`, `Number: 74219`, and `Unicode: 雪`.
5. Inspect real settled AI requests and included compute in the billing/usage UI.
6. Allow the demo session to idle-stop, resume it through Chrome, and observe both
   chats, their histories and all four files return with Connected status.
7. Upgrade only the retained demo to the rebuilt Fly runtime, upload
   `live-sdk-upload.txt` through the SDK, and observe `files.updated` on the runtime
   event stream. Chrome displayed the new file without refresh or chat activity,
   stayed Connected, and preserved an unsent composer draft. The real model then
   read the uploaded file and returned `LIVE_API_EVENT_OK`, `Color: violet`, and
   `Number: 92647` exactly.

Retained demo session: `01a0de95-db9c-74dc-b15e-584758defc1a`.
Chrome evidence: `/tmp/haarness-cloud-files-chats-proof.png`.
Other local evidence lives under `/tmp/haarness-*` and
`target/cloud-tools-live/sessions-chats-schedules/` (not published).
The live browser used the existing Vite working tree, including the separate UI
redesign. Only the small file-refresh change and its test are included in the Cloud
commit; the exact staged frontend was separately exported and tested/built.

## Boundaries and running environment

The native file-picker automation was blocked because Chrome's ChatGPT extension
has file-URL access disabled. The user was asked to enable it. API uploads, UI file
creation/edit/download/rename, attachment unit coverage and real model reads passed;
the native picker itself is not certified. Live Google/Stripe production flows and
external production deployment are not claimed; identity/payment tests use fixtures.

The local Docker Cloud API and worker run the new schedule backend with migration
0508, and the Vite frontend remains available for the user. The native CLI was
rebuilt. The development runtime image was built and pushed as
`registry.fly.io/haarness-workspace-orex@sha256:45f95975fdf9bf6538dd083da334a6ccf576b4cf85abf50937e9c46b02be51b0`.
Both ignored development image pins were updated, then the API/worker were
recreated and their readiness verified. Existing sessions retain their pinned image
on a plain resume; only the retained demo was stopped, saved with its unchanged
configuration/current version through the normal API, and restarted on the new
image. Its new file event was verified end to end in the SDK and Chrome. Other
existing sessions were not upgraded or stopped. Build instructions now explicitly
select the Docker ignore file, excluding credentials and local build artifacts.

The disposable fixture stack and its PostgreSQL container are removed after tests;
existing development services, user sessions and unrelated checkout changes are
preserved. The retained Chrome demo has a five-minute idle timeout and a 1 GB disk.
