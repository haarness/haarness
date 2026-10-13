# Server API v1

> **User guides:** [Your first API call](https://haarness.ai/docs/server/api), [Connect safely](https://haarness.ai/docs/server/authentication). This page has the engineering detail.

The unpublished [TypeScript and Python SDKs](../sdks/README.md) expose all
registered API operations. Their [complete operation inventory](../sdks/OPERATIONS.md)
and [generated OpenAPI document](../sdks/openapi.json) describe every route; a
running server's `/openapi.json` serves the same routes with its live schemas.

Portable profile catalog endpoints are documented in [Profiles](PROFILES.md#api)
and `/openapi.json`. Uploads store immutable archives without applying server
configuration; catalog operations use the configured server authentication.

Cross-project history: authenticated `GET /v1/stats` returns a versioned analytics
report; `GET /v1/insights` returns a text summary and standalone HTML. Both accept
`days`, `project`, `currentProject` and `limit`, cover the server's shared data root,
and publish schemas in OpenAPI. See [Insights](INSIGHTS.md) for accounting rules.

The server binds loopback by default. Explicit `haarness serve` accepts anonymous
requests unless a password or legacy token is configured. On a network address
(`--host 0.0.0.0`) it refuses to start without a password or token unless
`--anonymous` says that unauthenticated access is intended. Password servers use
`Authorization: Basic <base64(username:password)>`, with UTF-8 credentials and the
default username `haarness`. All protected routes, event streams, management APIs,
remote project gateway requests and terminal upgrades use the same authentication
policy. Failed password authentication returns `401` and a Basic challenge.
Managed local daemons and existing integrations retain `Authorization: Bearer <token>`;
their private runtime token also lets a password-protected host route internally.

Every mode retains Host/Origin validation. Loopback listeners require loopback
Host and Origin hosts; network listeners require matching browser Origin/Host.
`/healthz` and `/openapi.json` remain public diagnostics. Use HTTPS or an SSH tunnel
when connecting Desktop to a remote host. Passwords are optional configuration,
not a login/account service, and the server never accepts a password as a bearer token.

Deferred prompts use `delivery: "queue"` on `/input`, with revisioned `GET|POST /v1/threads/{id}/input/queue` controls. See [Steer and Queue](INPUT-QUEUE.md#api-and-sdks) for delivery, retries, events, and SDK usage.

Primary resources:

- CLI-hosted daemons also expose authenticated remote project discovery and routing: `GET /v1/host/projects`, `GET /v1/host/directories?path=...`, `POST /v1/host/open`, `DELETE /v1/host/projects/{workspaceId}` (closes a project for remote access), and `/v1/host/workspaces/{workspaceId}/v1/...`. Both take the catalog's workspace id (`workspaces[].id`), not `projects[].id`. These optional host routes are separate from the directory SDK/OpenAPI contract; the routed directory API uses the normal SDK with a project base URL. See [remote host architecture](DESKTOP-PROJECT-MODEL.md#remote-hosts-with-multiple-projects).

- `POST /v1/providers/refresh` refreshes connected model catalogs (same response as `GET /v1/providers`; `providers.refresh.v1` capability). See [Model catalogs](MODELS.md).
- `GET /v1/info`, `/v1/providers`, `/v1/agents`, `/v1/commands`, `/v1/skills`, `/v1/plugins`, `/v1/mcp`, `/v1/tools[?threadId=...]`
- `GET /v1/lsp` for configured server lifecycle/availability and `GET /v1/formatter` for configured formatter availability (`lsp.v1` and `formatter.v1` capabilities)
- `POST /v1/tools/{name}` with `{enabled:true|false|null}` for a durable workspace override; `POST /v1/tools/lazy` with `{mode:"off"|"auto"|"always"|null}` (null or omitted resets to configured/default mode)
- `GET /v1/projects`; `GET|POST /v1/workspaces`; `GET /v1/workspaces/refs`; `POST /v1/workspaces/resolve-ref`; `GET|DELETE /v1/workspaces/{reference}`; `POST /v1/workspaces/{reference}/open`, `/reset`, and `/cleanup`
- `GET|POST /v1/threads`, `GET|PATCH /v1/threads/{id}`
  Thread responses include `isSubagent` (default `false` for older payloads). Delegated child creation sets it; forks preserve `parentId` lineage with `isSubagent: false`. List APIs continue returning all threads; clients can filter sidebar presentations without losing child access.
  Untitled root conversations generate a title asynchronously from the first real user message after prompt hooks accept it. A `thread.updated` event publishes the persisted title while the main turn may still be running or waiting for permission. Titling is independent of turn completion and never overwrites a manual rename.
  Until then a chat is titled `New chat`. Servers before 2026-10 stored `New thread`; such rows are returned as `New chat` and still get a generated title, and a create request whose title is either placeholder or only spaces gets `New chat`. A fork without a `title` is named `Fork of <source title>`, or `Fork of New chat` when the source has none.
- `GET /v1/threads/{id}/archive[?sanitize=true]`; `POST /v1/session-archives/import`
- `GET|POST /v1/ptys`; `GET|PATCH|DELETE /v1/ptys/{id}`; WebSocket `GET /v1/ptys/{id}/connect?cursor=N`
- `POST /v1/threads/{id}/fork`, `/turns`, `/commands`, `/compact`
- `GET /v1/threads/{id}/items`, `/todos`, `/active`, `/background`, `/pending-question`, `/goal`
- `GET /v1/threads/{id}/diff` for the last completed turn that changed files and is not undone; `POST /v1/threads/{id}/revert` and `/unrevert` for stepwise undo/redo
- `GET /v1/vcs/diff?mode=working|branch[&context=N][&summary=true][&path=FILE]` for bounded Git review, metadata-only listings, or one literal file. Entries are `ReviewFile` (a `FileDiff` plus optional `truncation` and `omittedFiles`); bounds are 1000 files, 512 KiB per patch and 8 MiB per response — see [large-diff review](LARGE-DIFF-REVIEW.md)
- `GET /v1/threads/{id}/diff/review[?summary=true|path=FILE]` for deferred last-turn review; see [large-diff review](LARGE-DIFF-REVIEW.md)
Scheduled thread creation accepts optional `schedule: {id, name}`. The scheduler sets this durable origin on each new run; thread responses, live events, reloads and session archives retain it independently of the editable title. The schedule ID, rather than its name, is the grouping identity. Forks omit it. Sanitized archives redact the schedule name while retaining its ID. Existing threads without this additive field are unchanged.

Thread creation also accepts an optional client-chosen `id` (a UUID), so a create that is retried after a lost response cannot make a second chat. When a chat with that `id` already exists, `POST /v1/threads` returns it unchanged with the same `201` response, ignores the rest of the request, and publishes no second `thread.created`. Concurrent creates with one new `id` make one chat. Haarness Cloud uses this to deliver a session's first prompt into the chat it named in advance. Servers advertise it as `thread-create-id.v1` in `/v1/info.capabilities`; older servers ignore `id` and choose their own, so check the capability or compare the returned `id`.

- `POST /v1/threads/{id}/background[?childThreadId=...]` to promote running foreground subagents
- `POST /v1/threads/{id}/interrupt` to stop every active turn, child task, and workflow in that root conversation. Add `?resume=true` for an interactive Stop: steering the agent has not answered yet is answered by a continuation turn (see [Steer and Queue](INPUT-QUEUE.md))
- `POST /v1/threads/{id}/goal` for tagged goal lifecycle actions
- `GET /v1/turns/active`, `GET /v1/turns/{id}`, `POST /interrupt`, `GET /wait`
- `GET /v1/permissions`, `/v1/questions`; `POST /v1/permissions/{id}`, `/v1/questions/{id}`
- `GET`/`PUT /v1/threads/{id}/approval-mode` for the chat's Auto/Review setting, which the server applies for every client (see [automatic approvals](AUTO-APPROVALS.md))
- `POST /v1/mcp/{name}`, `/reconnect`, `/disconnect`, and MCP tool list/call routes
- `POST /v1/plugins` for install/enable/disable/remove/marketplace actions
- `GET /v1/workflows`, `/v1/workflows/{invocation}/source`, `/v1/workflows/runs`, and `/v1/workflows/runs/{id}`
- `POST /v1/workflows/run`, `/v1/workflows/runs/{id}/cancel`, `/pause`, and `/resume`; `GET /wait`; `DELETE /v1/workflows/runs/{id}`
- `GET /v1/events`
- `POST /v1/shutdown` for authenticated, explicit server-wide shutdown

`GET /v1/info` includes optional `defaultProvider` and `defaultModel` preferences
from the active workspace configuration. Clients can combine these with the
authenticated provider catalog, agent model overrides and recent choices to show
a concrete model before creating a thread. Missing fields remain compatible with
older servers; no credentials are included in these preferences.

Tool dispatch accepts a model's `functions.` namespace and unambiguous ASCII capitalization differences. Exact registered names take precedence, including names containing that prefix. Ambiguous case matches, spelling mistakes, and other namespaces are not guessed. Resolution uses the full registry before availability and permissions, so a disabled exact name cannot fall back to another tool. The canonical name passes through the same advertised-tool, availability, permission, hook, and snapshot checks. Batch planning applies the same resolution, rejects recursive batches, and records invalid arguments, unknown names, and unavailable tools as individual failures while continuing other well-formed calls.

The runtime validates tool arguments against the tool's execution JSON Schema before permission prompts or execution, including direct host calls and tools handled internally by the runtime. Hook-rewritten arguments are validated again. Errors identify the field path and expected constraint without repeating rejected values, and ask the model to correct the input. Validation does not coerce arguments or invent missing values. Native execution schemas retain existing compatibility behavior (shell `timeout_ms`, todo field aliases/default priority, and task continuation placeholders); the provider still sees the preferred input shape. A broken tool schema is reported as a tool-definition error. Unknown-name errors suggest currently available names; deferred-tool errors give the exact `load_tool` request and require the following model round; disabled tools instruct the model not to retry or change the spelling.

Desktop message actions and the TUI message picker offer assistant replies only.
They fork with `{upToSequence}` to include the selected reply and exclude later
messages. User messages retain Copy in Desktop. Whole-session forks remain available.
The API retains `{title?, beforeItemId?}` for older integrations that use an
exclusive user-message boundary; clients may send both boundary fields only
when the sequence is immediately before the named message. Forking is one
SQLite transaction: copied items receive new IDs and internal
`QueueConsumed.itemId` references are remapped to those IDs.
New forks also persist optional `forkOrigin: {title, sequence, itemId}` on the
thread in that transaction. `parentId` identifies the immediate source;
`title` is its title at fork time, `sequence` is the last item actually copied,
and `itemId` is that item's ID in the source. An empty fork records sequence
zero and a null item ID. Ordinary threads and older forks omit `forkOrigin`.
Desktop places a source link at this boundary and resolves the current source
title when available. Portable archives retain the boundary and title snapshot;
sanitized exports redact the title snapshot too.

Portable session archives use the strict `haarness.session-archive` JSON
format with `schemaVersion: 1`, advertised by the `session-archive.v1`
capability. Export includes the selected root, its complete descendant tree,
stable thread/turn/item IDs, visible transcript items, and goal/loop state.
Host-local workspace snapshots, revert state, credentials, permission rules,
and runtime settings are excluded. Active turns and child task states
are restored as interrupted; active goals are restored paused and awaiting
user input. Import is transactional and idempotently merges existing matching
IDs while rejecting ownership or sequence conflicts. `sanitize=true` replaces
titles, transcript content, attachments, tool arguments/results, goal text,
and loop prompts with redaction markers before the archive leaves the server.

Compaction accepts an optional caller `summary`; omitting it asks the server to
generate a real summary with the thread model. Before committing, the server
confirms any pending revert, captures a versioned exact todo checkpoint, and
records the configured recent-turn tail. Subsequent provider requests receive
the summary, exact todo JSON as data, then the retained turns. Media from the
pre-compaction tail is represented by a textual attachment note rather than
re-uploaded. Automatic compaction uses the selected model's context and output
limits and retries pre-output context-overflow/HTTP-413 failures after safe
compaction, under a bounded per-turn recovery ceiling.

Workspace create accepts `{name, fromRef?, branch?, path?}` and always creates
a unique, non-tracking Git branch. `GET /v1/workspaces/refs` discovers local
branches, remote branches, and commit-backed tags from the owning project;
`POST /v1/workspaces/resolve-ref` accepts `{reference?}` to validate custom
tags, branches, or commit IDs. Missing and blank references resolve to the same
stable primary-checkout branch (or exact detached commit) used by create. Reset accepts
`{confirm, discardChanges?, removeUntracked?, toRef?}`; removal accepts
`{confirm, discardChanges?, deleteBranch?}`. Stale registration cleanup uses
the separate `/cleanup` endpoint with `{confirm}`; it returns `409 conflict` if
the checkout path currently exists and never invokes Git or deletes a path.
`workspace-cleanup.v1` advertises this version-safe endpoint so older attached
servers cannot interpret cleanup as ordinary removal. The server returns `409 conflict`
while the target workspace owns an active server lease, and never resets or
removes an unmanaged/local checkout. Each workspace server exposes only that
workspace's sessions because its SQLite database, discovery record, token, and
lease are namespaced by durable workspace ID. `projects.v1` and `workspaces.v1`
in `/v1/info.capabilities` advertise this contract.

Interactive PTYs are process-local resources owned by one workspace server.
Create accepts optional `title`, `command`, `args`, and workspace-contained
`cwd`, plus positive `cols` and `rows`; omitting the command starts the user's
shell. Patch changes the title and/or resizes using both dimensions. A WebSocket
client sends raw input as binary frames and receives raw output as binary
frames. JSON text control frames mark replay boundaries and acknowledge the
monotonic byte `cursor`; reconnecting with the last acknowledged cursor replays
the missing range from a bounded two-megabyte buffer. Lifecycle changes also
publish `pty.created`, `pty.updated`, `pty.exited`, and `pty.deleted` SSE events.
Disconnecting a socket never terminates its process. DELETE does, as does
graceful server shutdown. PTYs intentionally do not survive server restart and
their output is never persisted in SQLite. `pty.v1` advertises this contract.

SSE envelopes include `schemaVersion`, `serverInstanceId`, monotonic `eventId`, type, relevant domain IDs, payload, and timestamp. `GET /v1/info` exposes `eventCursor`: snapshot-first clients capture it before REST hydration and connect with that value as `Last-Event-ID`, closing the snapshot/delta race. Reconnects continue from the last applied event. A changed server instance or `resync_required` means clients must replace items, todos, the process-wide `/v1/turns/active` snapshot, pending requests, and thread snapshots from REST. SSE connections are observers: disconnecting, switching threads, or creating a new thread never cancels execution. Pending questions are root-scoped structured records (question, child thread, root thread, turn, choices, and creation time); `question.requested` adds one and `question.resolved` removes exactly that ID, so parallel child questions never overwrite each other. A question must be answerable: the `question` tool rejects a call that turns free-form answers off (`custom: false`) without offering a choice.

Runtime-source changes publish `configuration.reload_queued` when an active
turn delays application, `configuration.reloaded` after the complete new
generation is live, and `configuration.reload_failed` when validation rejects
the candidate. `assets.updated` follows a successful reload and tells clients
to refresh provider, command, agent, workflow, skill, MCP, and language-service
catalogs. The `serverInstanceId` does not change during a hot reload.
A reload follows source content, not file events: rewriting a file unchanged,
editor scratch files (`*.swp`, `*.swo`, `*.swx`, `*~`, `.#*`, `#*#`, `4913`,
JetBrains `___jb_tmp___`/`___jb_old___`, `.DS_Store`), folder timestamps, and
an OAuth token refresh that only rewrites `access`, `refresh`, and `expires` in
`auth.json` reload nothing. Sources that settle back on the live generation's
content reload nothing either. A reload keeps each connected MCP server whose
effective configuration (with `${VAR}` references resolved) is unchanged,
instead of starting it again. Servers connect concurrently at startup and on
reload.
An edit that changes only agent definition files (markdown in an `agents` or
`agent` folder) does not wait for active turns: once the files load, new
turns use the new definitions and `assets.updated` (`resource: "agent"`) is
published, with no `configuration.reload_queued`. A turn keeps the agent
definition it was admitted with until it ends, and a delegated child the one
it was dispatched with, however the agent is edited, renamed or deleted
meanwhile, from a file, the management tool, Desktop or the TUI. An agent
file whose frontmatter does not parse makes the change wait for idle like any
other. Skill edits still wait for idle: skill bodies are cached in the
registry and the `skill` tool's catalog is part of the generation.
Servers advertise this contract as `configuration-hot-reload.v1` in
`/v1/info.capabilities`.

The `task` tool accepts `prompt`, optional `agent` and `description`, and an
optional `background` boolean. A new call creates one durable child thread and
turn; `task_id` continues an existing child belonging to that parent, and queues
new context when that child is still running. Foreground calls wait for that child; background calls return a running
envelope immediately. A foreground child can be promoted through the
`/background` endpoint without cancellation or restart. `GET /background`
lists both promotable foreground children and already-background children.
Terminal state is persisted as a `TaskState` transcript item and a background
completion is queued back into the parent automatically after its launch tool
result is durable. The registry itself is process-local: restart recovery
interrupts orphaned work rather than replaying it.

The server is the sole execution owner. Graceful server shutdown interrupts
active turns/workflows into durable terminal states after a bounded drain.
Startup recovery marks orphaned queued/running turns interrupted and appends a
retryable transcript notice while preserving prior items; provider streams and
side-effecting tools are never replayed speculatively. Active goals recover as
paused with `awaitingUserInput=true`, so elapsed indicators cannot claim a
stale execution is still pursuing after process death.

Todo state is available through `GET /v1/threads/{id}/todos` and
`todo.updated` (`{threadId,todos}`). The canonical tools are `todowrite`
(`todos`) and `todoread`; the older `todo` (`items`) spelling remains accepted.
Entries carry `content`, `status` (`pending`, `in_progress`, `completed`, or
`cancelled`), and `priority` (`high`, `medium`, or `low`; old records default to
`medium`). Fork, revert, unrevert, and revert confirmation all reconcile the
current todo snapshot with transcript history.

Mutating turns and direct native workspace tools are enclosed by paired
before/after checkpoints. Undo applies the newest complete pair with conflict
checks, can walk backward repeatedly, and keeps redo available until new work
confirms the boundary. Redo advances one checkpoint at a time. An undo or redo
changes all of its files or none: when a path cannot be written part way (a
read-only directory, a locked file, a full disk), every path already changed
is put back, the chat keeps its previous undo state, and the same request
succeeds once the cause is fixed. Undo and redo run one at a time across the
chats of a workspace. The last-turn diff comes from the newest pair before any
pending undo boundary that changed files: an undone turn drops out of it, and a
later turn that only ran commands does not hide the edits before it. The VCS
endpoint reports staged, unstaged, untracked, unborn-branch, and
branch-relative changes without following workspace symlinks. Diff bodies and
process output are bounded.
Undo against an active turn cancels it and waits for its terminal checkpoint
before restoring; a cancellation-resistant operation times out without a file
restore. Text-only turns receive a same-snapshot boundary lazily, so undoing a
prompt never falls through to an older file mutation.

Goals are durable structured state, not injected system instructions.
`POST /v1/threads/{id}/goal` accepts tagged `status`, `pause`, `resume`,
`clear`, `complete`, `block`, and `set` actions; the legacy
`{goal:string|null}` body remains accepted. `GET` returns the same canonical
state/message envelope.
`goal.updated` carries lifecycle, accounting, criteria, blocker, tombstone,
and optional localized completion-action state. Goal timestamps are epoch
milliseconds on the wire (legacy RFC 3339 state remains readable), and paired
`goal.logical_turn.started` / `goal.logical_turn.finished` events expose every
provider-facing pursuit boundary even though Haarness retains one durable user
turn. Active root turns receive the goal as a hidden user-role doctrine and may
use `get_goal`, `update_goal`, and `propose_goal_criteria`; the runtime owns
token/time budgets, continuation limits, multi-select/custom criteria approval,
criteria evidence checks, and provider-limit transitions. Goal objectives are
bounded to 4,000 characters with an explicit `[truncated]` marker.

`/openapi.json` combines the live `haarness-protocol` schemas with every route
from the generated [sdks/openapi.json](../sdks/openapi.json), listing what it
filled in from that file under `x-haarness-reference`, and is the
desktop-client compatibility contract.

`GET /v1/commands` is the authoritative executable slash-command registry.
Names resolve case-insensitively and collisions use one explicit precedence:
the reserved `workflow` and `goal` managers, then configured prompt commands,
then direct discovered workflow invocations. Collision losers are omitted, so
every advertised entry is executable by `POST /v1/threads/{id}/commands`.

MCP configuration accepts stdio, Streamable HTTP, and legacy SSE transports.
Remote entries may set `oauth`, `oauthClientId`, and `oauthScopes`; bearer and
refresh credentials never appear in the v1 wire configuration. A protected
server reports `needs_auth` with a non-secret detail until the CLI completes
the public OAuth authorization-code + PKCE flow. `disconnect` drops the live
worker and advertised tools without deleting configuration; `reconnect`
performs initialization and capability discovery again.
`timeoutMs` (default 30 s) bounds the socket connect, initialization, and tool
discovery as one deadline, plus a `headersHelper` run. It does not bound tool
calls or the Streamable HTTP response streams that carry their results; a
timed-out connection names its limit ("MCP connection timed out after 30 s").
Tool descriptions are capped at 2048 characters, ending in `… [truncated]`;
set `HAARNESS_MAX_MCP_DESCRIPTION_LENGTH` to a positive character count to
change the budget. A tool schema without a root `type` gets `"object"`, as MCP
requires. Requests to Claude (Anthropic Messages, Bedrock, and Haarness Cloud
routers) omit any tool whose root input schema has `oneOf`, `anyOf`, or `allOf`,
or a root `type` other than `object`, which Claude rejects for the whole
request. A WARN line names each such tool once per process.
Connected servers expose tool list/call, resource and resource-template lists,
resource reads, prompt lists, and prompt resolution through the corresponding
`/v1/mcp/{name}/...` routes. Unsupported prompt/resource capabilities return
an empty list rather than probing an undeclared server method.
That POST returns a tagged `turn`, `workflow_run`, or `text` result and leaves
the stable `POST /turns` contract unchanged. A workflow hidden by a configured
direct command remains explicitly available as `/workflow run <name>`.

Turn and queued-input requests accept an `attachments` array containing local
PDF/image/audio/video paths, `file:` URLs, or base64 `data:` URLs. The legacy
`images` key remains a read alias. The server persists references; native
provider adapters validate, read, size-limit, and encode them when building
each multimodal request. Tool results use the same representation through a
bounded `metadata.attachments` array, allowing `read`, MCP media results, and
generated assets to survive persistence and reach the next provider round.
Explicit attach clients convert their local media into bounded data URLs before
sending, with a display-only encoded filename parameter. This keeps remote
attachments independent of the server filesystem while retaining readable
transcript chips. Exact support remains provider-specific: OpenAI, Anthropic,
and Bedrock accept PDF/images; Gemini accepts PDF/image/audio/video; compatible
Chat gateways receive file/image/MP3/WAV shapes where supported.

Ultracode is an additive, provider-neutral workflow mode. `Thread.ultracode`
stores the standing session preference; `CreateThreadRequest.ultracode` and
`PATCH /v1/threads/{id}` update it. Every `Turn` records the effective
`ultracode` boolean so restarts, forks, child tasks, and clients preserve the
decision. A `StartTurnRequest` may use the symbolic `reasoningEffort:
"ultracode"` for a one-shot turn without mutating its thread. A standalone,
case-insensitive `ultracode` in the submitted request text activates the same
one-shot mode; Unicode identifier neighbors and `/ultracode` are excluded. At
provider-call time the server resolves that symbol from `ModelInfo.reasoningEfforts`, choosing
the strongest advertised effort (with the Claude-compatible `xhigh` cap); an
empty list deliberately leaves provider effort unset. Ultracode turns also
receive workflow-orchestration system guidance independently of the provider.
Outside Ultracode, the model-facing `workflow` tool is advertised only when
the request includes standalone `workflow`/`workflows` wording or explicitly
sets `tools.workflow=true`. Child-agent turns require that explicit opt-in;
direct workflow commands and API starts are unaffected.

Dynamic workflow starts accept exactly one of `name`, inline `source`, or
explicit `path`, plus optional JSON `args`, workflow-wide `agent`, `model`, and
`variant`, launching `threadId`, `wait`, and bounded `timeoutMs`. Named definitions come from the static project/global/
plugin catalog. Each run executes in a dedicated child session and persists an
immutable source snapshot and SHA-256, status, phase, structured status,
ordered logs, warnings, child-agent records, result/error, and timestamps.
`GET /v1/workflows/runs/{id}?afterSequence=N` returns only logs newer than the
given sequence. `GET /v1/workflows/runs?threadId=...` scopes recent runs to the
conversation that launched them (with execution-thread matching retained only
for legacy rows that predate launch-session persistence); the wait endpoint
accepts a bounded timeout. Pause/cancel stop
the active execution, and resume restarts paused/interrupted source from the
beginning. `workflow.updated`, `workflow.log`, and `workflow.deleted` are live
SSE event types. See [WORKFLOWS.md](WORKFLOWS.md) for source formats and the
security boundary.

## Interactive configuration and sharing

`POST /v1/manage/{resource}` accepts an authenticated JSON action and returns a
`ManagementView` (`title`, `lines`, `fields`, `entries`, optional `submit`).
Resources are `providers`, `mcp`, `schedules`, `hooks`, `memory`, `profiles`,
and `sharing`. Begin with `{"action":"view"}`; sharing also requires
`threadId`. Render fields, preserve the returned submit/revision metadata, merge
edited values into that submit object, and send it to the same resource. An
entry's `confirm` text requires explicit user confirmation before its request is
submitted. `secret` fields must use masked input and never be copied to logs.

TypeScript exposes `manageResource(resource, body)`; Python exposes
`manage_resource(resource, body)`. These are user-management APIs, separate from
model tool execution. Errors leave the client draft intact.

`POST /v1/manage/assets` serves the structured definition editors. Send `kind`
(`agent`, `command`, or `skill`) and `action` (`list`, `get`, `create`, `edit`,
`customize`, or `delete`). It returns definition rows/details or a mutation result,
rather than a `ManagementView`. Writes use the connected server's configuration;
`scope` is `global` or `local`. `customize` creates a same-name override from a
built-in or inherited definition, retains agent permissions and skill support
files, and refuses to overwrite an existing custom definition. Returned
`customized` metadata identifies overrides; deleting one restores the underlying
definition. Plugin files remain unchanged. Changes trigger the normal runtime
configuration refresh. This endpoint uses the same authentication as other
management APIs and is routed through the selected remote workspace.

Sharing's preview action stores a private, immutable text preview. Its create
action publishes only that reviewed preview and returns `/s/{token}`; resolve
this against the server host origin, outside a project gateway path. The public
HTML route needs no bearer token. Links expire and can be revoked through the
authenticated sharing manager. Sharing does not publish session credentials,
attachments or a restorable archive. Preview text itself must be reviewed for
secrets. See [interactive parity and verification](records/2026-09-12-tui-parity.md)
for client workflows, permission handling and verification evidence.
