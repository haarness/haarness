# Server API v1

The HTTP, SSE and WebSocket API of a Haarness server (`haarness serve`, and the server the TUI and Desktop
start for each folder): authentication, every resource group, events, capabilities and the contracts behind
them. It is for developers building clients or integrations and for contributors changing the server.

> **User guides:** [Your first API call](https://haarness.ai/docs/server/api), [Connect safely](https://haarness.ai/docs/server/authentication), [TypeScript and Python SDKs](https://haarness.ai/docs/server/sdks).

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

The server binds loopback by default: `haarness serve` listens on `127.0.0.1:4096`
(a free port while another program holds 4096; `--port 0` always picks one).
Explicit `haarness serve` accepts anonymous
requests unless a password or legacy token is configured. On a network address
(`--host 0.0.0.0`) it refuses to start without a password or token unless
`--anonymous` says that unauthenticated access is intended. Password servers use
`Authorization: Basic <base64(username:password)>`, with UTF-8 credentials and the
default username `haarness`. All protected routes, event streams, management APIs,
remote project gateway requests and terminal upgrades use the same authentication
policy. Failed password authentication returns `401` and a Basic challenge.
A token (`--token` or `HAARNESS_SERVER_TOKEN`) makes the server require
`Authorization: Bearer <token>`. The folder servers the TUI, Desktop and
`haarness run` start for themselves (on a free loopback port) get a random
private token, kept in the folder's private runtime discovery record where
local clients find it; that token also lets a password-protected host route
internally.

A `haarness serve` started while the folder's server runs is a front for it: it
judges Host, Origin and credentials for its own address, then forwards `/v1` requests
with the server's token (and the paths that need none without it). `POST /v1/shutdown`
at a front closes the front only. A request a front admitted without credentials
reaches the server with `x-haarness-anonymous: 1`, and the server treats it as
anonymous; any client may send that header to give up access.

Every mode retains Host/Origin validation, judged before credentials (a browser
sends a saved password, and an open server needs none). Loopback listeners require
a loopback Host, and a browser Origin on a loopback name with the server's own port:
a page on another local port (a dev server at `http://localhost:3000`) is another web
site. A request with no Origin that a page started (an
image, link, frame or `no-cors` fetch, which browsers mark with a `Sec-Fetch-Site`
of `same-site` or `cross-site`) gets `403`; only an absent `Sec-Fetch-Site`, `none`
(the user's own navigation) or `same-origin` passes. On network listeners a browser
request (an Origin, or a `Sec-Fetch-Site` other than `none`) must name the server
by IP address or `localhost` in Host, so a web site whose DNS name was re-pointed at
the server (DNS rebinding) is refused, and its Origin must match that Host, port
included. The CLI, TUI, Desktop (its main process makes the requests), SDKs and the
Cloud relay send neither header, so a reverse proxy may name the server however it
likes for them. Not covered: a browser that sends no `Sec-Fetch-Site` (Safari before
16.4, Firefox before 90) makes a same-origin GET without an Origin, so after DNS
rebinding a page in it can still read GET responses from a `--anonymous` network
server. Requiring an IP address in Host for every request would close that, but would
break `haarness attach` to a LAN host by name; use a password on network listeners.
`/healthz` and `/openapi.json` remain public diagnostics. Use HTTPS or an SSH tunnel
when connecting Desktop to a remote host. Passwords are optional configuration,
not a login/account service, and the server never accepts a password as a bearer token.

Primary resources:

- CLI-hosted daemons also expose authenticated remote project discovery and routing: `GET /v1/host/projects`, `GET /v1/host/directories?path=...`, `POST /v1/host/open`, `DELETE /v1/host/projects/{workspaceId}` (closes a project for remote access), and `/v1/host/workspaces/{workspaceId}/v1/...`. Both take the catalog's workspace id (`workspaces[].id`), not `projects[].id`. `GET /v1/host/directories` takes `path`, `prefix`, `hidden`, `limit` and `after`. The four host routes are in the SDKs and the generated OpenAPI (`hostProjects`, `hostDirectories`, `hostOpen`, `hostClose`); the forwarded `/v1/host/workspaces/{workspaceId}/…` API is the ordinary project API, which the SDKs' `workspace(id)` helper binds every method to. A request forwarded to a project whose running server is busy and would have to be replaced (another Haarness build, or the other project-configuration setting) is answered `409` with code `project_busy`; plain reads still reach a server of another build that speaks this API. See [remote host architecture](DESKTOP-PROJECT-MODEL.md#remote-hosts-with-multiple-projects).

- `POST /v1/providers/refresh` refreshes connected model catalogs (same response as `GET /v1/providers`; `providers.refresh.v1` capability). See [Model catalogs](MODELS.md).
- `GET /v1/info`, `/v1/providers`, `/v1/agents`, `/v1/commands`, `/v1/skills`, `/v1/skills/{name}` (a skill's instructions), `/v1/plugins`, `/v1/mcp[?threadId=...&agent=...]`, `/v1/tools[?threadId=...]`; a `/v1/mcp` server of the [system folder](CONFIGURATION-SCOPES.md#system-folder) has `scope: "system"` (absent otherwise). Each `/v1/tools` entry carries its functional `group` and `groupTitle` (`core`, `agents`, `planning`, `memory`, `goals`, `cloud`, `sessions`, `automation`, `web`, `media` or `extensions`), its `source` (`native`, `extension:<name>` or `mcp:<server>`), and whether it is `enabled`, `essential`, `advertised` on the next round and `loaded`, with token estimates for the list; Desktop's Settings → Tools groups by source from these
- `GET /v1/lsp` for configured server lifecycle/availability and `GET /v1/formatter` for configured formatter availability (`lsp.v1` and `formatter.v1` capabilities)
- `POST /v1/tools/{name}` with `{enabled:true|false|null}` for a durable workspace override; `POST /v1/tools/lazy` with `{mode:"off"|"auto"|"always"|null}` (null or omitted resets to configured/default mode)
- `GET /v1/projects`; `GET|POST /v1/workspaces`; `GET /v1/workspaces/refs`; `POST /v1/workspaces/resolve-ref`; `GET|DELETE /v1/workspaces/{reference}`; `POST /v1/workspaces/{reference}/open`, `/reset`, and `/cleanup`
- `GET|POST /v1/threads` (`?archived=true` lists archived chats), `GET|PATCH /v1/threads/{id}`
  Thread responses include `isSubagent` (default `false` for older payloads). Delegated child creation sets it; forks preserve `parentId` lineage with `isSubagent: false`. List APIs continue returning all threads; clients can filter sidebar presentations without losing child access.
  Untitled root conversations generate a title asynchronously from the first real user message after prompt hooks accept it. A `thread.updated` event publishes the persisted title while the main turn may still be running or waiting for permission. Titling is independent of turn completion and never overwrites a manual rename.
  Until then a chat is titled `New chat`. Servers before 2026-10 stored `New thread`; such rows are returned as `New chat` and still get a generated title, and a create request whose title is either placeholder or only spaces gets `New chat`. A fork without a `title` is named `Fork of <source title>`, or `Fork of New chat` when the source has none.
- `GET /v1/threads/{id}/archive[?sanitize=true]`; `POST /v1/session-archives/import`
- `GET|POST /v1/ptys`; `GET|PATCH|DELETE /v1/ptys/{id}`; WebSocket `GET /v1/ptys/{id}/connect?cursor=N`
- `POST /v1/threads/{id}/fork`, `/turns`, `/commands`, `/compact`
  A turn needs text or an attachment: `/turns` refuses text that is empty or only whitespace with neither an attachment nor a `command` (`400`, "text or an attachment is required"), as queued input always did, and the `task` tool refuses a blank `prompt`. Claude refuses whitespace-only text, and a blank message kept in the chat would fail every later turn. For the same reason, a reply that is only whitespace (the line breaks a model can stream before a tool call) is kept in the chat but never sent back to a model, and the Anthropic and Bedrock adapters leave out blank text next to an image or a document (Bedrock, which needs text beside a document, gets `(document attached)`, and an empty tool result reads `(no output)`). A chat that already runs a turn refuses `/turns` with `409 conflict` and the message "This chat already has a turn running, started in another window or run. It takes a new message once that turn finishes or is stopped." (`haarness run --session` and editors over ACP show it as it is); `/input` steers a message into the running turn instead.
  A client that shows the conversation to a person with a Stop control sends `attended: true` with `/turns`, `/input` (kept with a queued message until it runs) and `/commands`, as Desktop, the terminal UI and an editor over ACP do: a command's or skill's `` !`…` `` snippet in the turn that starts, and in the subagents it starts, then runs until it finishes or the turn is stopped. Without it (a script, `haarness run`, an SDK call, Haarness Cloud, an older client) a snippet ends at `execution.shell_timeout_ms` as a model's shell call does; a scheduled run keeps that deadline whatever is sent, and so do turns Haarness starts itself (a background result, a goal's next step, a workflow's agent, a `/loop` prompt). The field is optional and changes nothing else; servers before it ignore it.
- `POST /v1/threads/{id}/input` steers a message into the running turn, or with `delivery: "queue"` keeps it for after; revisioned `GET|POST /v1/threads/{id}/input/queue` lists and changes what is queued (`input-queue.v1`; see [Steer and Queue](INPUT-QUEUE.md#api-and-sdks) for delivery, retries, events and SDK usage)
- `POST /v1/threads/{id}/btw` asks a side question about the chat without adding to it; `POST /v1/threads/{id}/btw/stream` streams its answer as SSE (`btw.stream.v1`; see [side questions](SIDE-QUESTIONS.md#api))
- `GET|POST /v1/threads/{id}/loop`: `{prompt, intervalSeconds}` (default 300, kept between 30 and 86,400) starts a recurring prompt in the chat, which expires after 7 days; `{stop: true}` ends it
- `POST /v1/threads/{id}/shell` with `{command}` runs one shell command through the `bash` tool and its permission path, recorded in the chat without a model turn (`409` while a turn runs)
- `GET /v1/threads/{id}/usage`: token and cost totals for the chat and, under `descendants`, its subagents, `latestInputTokens`, and `context` (`tokens`, `reportedTokens`, `estimatedTokens`, `window`, `compactAt`), the one context figure every client shows
- `GET /v1/threads/{id}/items`, `/todos`, `/active`, `/background`, `/pending-question`, `/goal`
- `GET /v1/threads/{id}/activity` → `{"activeTurn": Turn | null, "background": N, "lastTurn": Turn | null}` (all keys always present; `404` for an unknown chat): the turn running in the chat, how many background results it is still owed, and its newest turn by creation whatever its status (null for a chat without turns). `lastTurn` is read after the other two, so a consumer holding an earlier turn can tell a chat that went quiet with that turn as its newest (it was stopped while idle) from one where a follow-up turn started and ended between two reads. Work a chat started in the background (child agents launched with `background: true` or moved there, workflow runs started without waiting) delivers its result into that chat when it ends, starting a new turn when the chat is idle; work a stop cancelled owes nothing. A chat is quiet, and its last root turn is where it stopped, only with no active turn and `background` 0. A published chat's scope may read it for every chat in its tree. A background workflow run that is paused owes nothing until it is resumed. Servers before 2026-10-17 answer `404`; fall back to `/active` there.
- `GET /v1/threads/{id}/diff` for the last completed turn that changed files and is not undone; `POST /v1/threads/{id}/revert` and `/unrevert` for stepwise undo/redo
- `GET /v1/vcs/diff?mode=working|branch[&context=N][&summary=true][&path=FILE]` for bounded Git review, metadata-only listings, or one literal file. Entries are `ReviewFile` (a `FileDiff` plus optional `truncation` and `omittedFiles`); bounds are 1000 files, 512 KiB per patch and 8 MiB per response — see [large-diff review](LARGE-DIFF-REVIEW.md)
- `GET /v1/threads/{id}/diff/review[?summary=true|path=FILE]` for deferred last-turn review; see [large-diff review](LARGE-DIFF-REVIEW.md)
- Project files, every path resolved inside the project: `GET /v1/files/list?path=…[&hidden=true]`; `GET /v1/files/content?path=…` (`content`, `encoding`, `mime`, `revision`, `size`; at most 10 MiB) and `PUT` with `{path, content, revision, encoding?}` (`utf8` or `base64`; a new file uses the empty content's SHA-256 as its revision, and a stale revision is `409`); `POST /v1/files/action` with `mkdir`, `rename` (`target`) or `delete` (`confirm`). Changes publish `files.updated`. `GET /v1/files/search?q=…[&limit=N]` (default 8, at most 50) is the `@` mention search
- `GET /v1/files/preview`, `/v1/files/preview/current` and `POST /v1/files/preview/restore` serve the saved versions of [canvas previews](CANVAS-PREVIEW.md) (`files.preview.v1`)
- Browser hosts: `GET /v1/browser/hosts`, the host sockets `GET /v1/threads/{id}/browser/host`, `/v1/browser/drafts/{id}/host` and `/v1/browser/extension/host`, `POST /v1/threads/{id}/browser/tool`, the `…/tunnel/{action}` routes and `POST /v1/browser/drafts/{id}/adopt` (`browser.v1`, `browser-drafts.v1`, `browser-extension.v1`); see [Browser](BROWSER.md) and [Haarness for Chrome](CHROME-EXTENSION.md)
- `POST /v1/cloud/agents`, `/v1/cloud/workspaces`, `/v1/cloud/chats` and `/v1/cloud/schedules` act on Haarness Cloud with this computer's signed-in account, through the same intents as the model's Cloud tools

Scheduled thread creation accepts optional `schedule: {id, name}`. The scheduler sets this durable origin on each new run; thread responses, live events, reloads and session archives retain it independently of the editable title. The schedule ID, rather than its name, is the grouping identity. Forks omit it. Sanitized archives redact the schedule name while retaining its ID. Existing threads without this additive field are unchanged.

Thread creation also accepts an optional client-chosen `id` (a UUID), so a create that is retried after a lost response cannot make a second chat. When a chat with that `id` already exists, `POST /v1/threads` returns it unchanged with the same `201` response, ignores the rest of the request, and publishes no second `thread.created`. Concurrent creates with one new `id` make one chat. Haarness Cloud uses this to deliver a workspace's first prompt into the chat it named in advance. Servers advertise it as `thread-create-id.v1` in `/v1/info.capabilities`; older servers ignore `id` and choose their own, so check the capability or compare the returned `id`.

A chat can also be created with a folder of its own: `directory`, as `<date>/<name>` relative to the server's project folder (`YYYY-MM-DD`, then a name of at most 255 bytes with no `\`, `:`, control character or space at either end, not starting with `-` or `.`). The server makes it one part at a time, never through a link, and answers `400 invalid_request` with a plain message when the name has another shape, would lead out of the project folder, or a link stands in for a part. When another chat (archived ones too) already works in it, or a file or link has its name, the server takes `<name>-2`, `-3`, … instead; a folder that exists but no chat uses is taken as it is. Given instead as the absolute folder another chat reports, `directory` shares that folder as it is (a schedule's runs work in the chat it was made in this way). The chat's `Thread` then carries `directory`, the folder canonical and absolute, in every answer and event; chats without one leave it out. Its tools work there, with the folder as their project (see [Permissions and execution](PERMISSIONS-AND-EXECUTION.md#a-chats-own-folder)), so the paths of its tool calls, diffs and last-turn review are relative to it. Chats created under it (`parentId` without a `directory`: workflow chats and their agents, task subagents) and its forks carry the same folder. Session archives carry none: an imported chat works in the importing server's project folder, or in the chat home in a new folder of its own. On the chat home's server every chat has a folder: a create that names none and has no parent (from any client, a workflow started on its own, `haarness run`, ACP, the terminal UI) gets `<today>/chat-<last four characters of its id>`, and a chat stored without one gets one the first time it is used (a turn, a tool call, a review, a side question; its parent's folder when that has one), with `thread.updated`. `GET /v1/commands` and `GET /v1/workflows` take `?threadId=` to list what that chat can run too: a chat with a folder of its own also gets the workflows in its folder's `.haarness/workflows` (a `400` when a link stands in for that folder; a published chat may ask only for chats of its own tree). The file APIs take the same `?threadId=` (`GET /v1/files/search`, its `./…` and `../…` completion included, `GET /v1/files/list`, `GET|PUT /v1/files/content`, `POST /v1/files/action`): for a chat with a folder of its own they work inside that folder, confined to it as they are to the project folder (a link out of it, to another chat's folder too, is refused like an outside path), every path they take and return is relative to it, and a listing or a file says which folder in `directory`; a `files.updated` event then carries `threadId` and `directory` too. An unknown chat answers `404`, a malformed id `400`; without `threadId`, or for a chat without a folder, the project folder as before. Servers advertise it as `thread-directory.v1`; older ones ignore `directory` and create an ordinary chat.

- `POST /v1/threads/{id}/background[?childThreadId=...]` to promote running foreground subagents
- `POST /v1/threads/{id}/interrupt` to stop every active turn, child task, and workflow in that root conversation. Add `?resume=true` for an interactive Stop: steering the agent has not answered yet is answered by a continuation turn (see [Steer and Queue](INPUT-QUEUE.md))
- `POST /v1/threads/{id}/goal` for tagged goal lifecycle actions
- `GET /v1/turns/active`, `GET /v1/turns/{id}`, `POST /interrupt` (`?resume=true` as above), `GET /wait?timeout_ms=N` (default 30 s, at most 300 s; a turn still running then answers `409`)
- `GET /v1/permissions`, `/v1/questions`; `POST /v1/permissions/{id}`, `/v1/questions/{id}`.
  A pending permission names its `permission` category, `patterns`, the `alwaysPatterns` an Always would remember, `threadId`/`rootThreadId`/`turnId`, and `callId`, the tool call that asks (a batch's step as `<batch call id>:N`; older servers send none); `explicit: true` means only a person may answer it. A request a shell command raises carries `script` (the file the command runs, as read when it asked, or why its text is withheld), and one Auto's reviewer left to a person carries `review`. A question names its `callId` the same way. A permission reply is `{decision, feedback?}` with `once`, `always`, `always_workspace`, `always_repository`, `reject` or `refuse` (for unattended runners); a question reply is `{answer}`. See [Permissions and execution settings](PERMISSIONS-AND-EXECUTION.md) and [automatic approvals](AUTO-APPROVALS.md)
- `GET`/`PUT /v1/threads/{id}/approval-mode` for the chat's Review, Auto or Yolo setting, which the server applies for every client (see [automatic approvals](AUTO-APPROVALS.md))
- `POST /v1/mcp/{name}`, `/reconnect`, `/disconnect`, and MCP tool list/call routes
- `POST /v1/plugins` for install/enable/disable/remove/marketplace actions
- `GET /v1/workflows`, `/v1/workflows/{invocation}/source`, `/v1/workflows/runs`, and `/v1/workflows/runs/{id}`
- `POST /v1/workflows/run`, `/v1/workflows/runs/{id}/cancel`, `/pause`, and `/resume`; `GET /wait`; `DELETE /v1/workflows/runs/{id}`
- `GET /v1/events`
- `POST /v1/shutdown` for authenticated, explicit server-wide shutdown
- Public, without credentials: `GET /healthz`, `GET /openapi.json` and the shared-chat page `GET /s/{token}`

`GET /v1/info` includes optional `defaultProvider` and `defaultModel` preferences
from the active workspace configuration. Clients can combine these with the
authenticated provider catalog, agent model overrides and recent choices to show
a concrete model before creating a thread. Missing fields remain compatible with
older servers; no credentials are included in these preferences.
`defaultReasoningEffort` is the configured `defaults.reasoning_effort`, the level
a turn that names no effort runs at (the nearest level the model lists, or the
value as written for a model that lists none); it is absent when nothing is
configured. A turn may then name `reasoningEffort: "default"` to run at the
model's own default instead, and queued prompts, steering and background results
of that chat keep the choice. A server from before this field does not know the
name: its turn endpoint refuses it, while its input-queue and command endpoints
take it as an unknown level and run the turn at the model's strongest level (or
pass it on to a model that lists none). Clients therefore send it only to a
server that reports `defaultReasoningEffort`; elsewhere naming no effort means the
same.

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
compaction, under a bounded per-turn recovery ceiling. A refusal of the request
for its size in bytes (an HTTP 413, Bedrock's "413 Payload Too Large" or
Gemini's "Request payload size exceeds the limit", rather than a context too
long in tokens) compacts once until the provider takes a request again: when it
refuses the compacted request as well, the turn fails with one error saying the
files in the chat are too large to send together, and to continue in a new chat
or with smaller files, instead of summarizing the summary. Where the chat cannot
compact (a child agent, compaction turned off, input queued meanwhile) such a
refusal fails the turn with the same advice.

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
in `/v1/info.capabilities` advertise this contract. The chat home
(`haarness --chat-home`, the folder of chats without a project) is no project:
`GET /v1/projects`, `GET /v1/workspaces` and the host catalog leave it and its
own project out (`currentWorkspaceId` still names it on its own server), and
its `WorkspaceInfo`, where one is read, has `chatHome: true`; other workspaces
leave the field out.

Interactive PTYs are process-local resources owned by one workspace server.
Create accepts optional `title`, `command`, `args`, and workspace-contained
`cwd`, plus positive `cols` and `rows`; omitting the command starts the user's
shell. Instead of `cwd` (never with it) it accepts a chat's folder as
`directory`, `<date>/<name>` relative to the project folder or the absolute
folder its thread reports; it must be that chat folder, existing and no link, the shell starts there, and
the terminal says so in `directory` (canonical and absolute) in every answer
and event, so a client can show each chat its own terminals. With `ifNone: true` it starts one only when the project has no running
terminal (with `directory`: that folder has none; without it, terminals
opened for a chat's folder do not count) and otherwise answers with its oldest
running one (still `201`), so
several windows that each open a first terminal at once, as after the server
was replaced, share one shell: Desktop's terminal panel asks this way when it
finds none, and so does the terminal UI's first `/terminal`. Every terminal
says `created`, true only in the answer to the create that started it (false
in lists, events and a handed-back terminal), so a client knows whether the
shell's output is all new to it or belongs to a shell another one started; a
server too old to send the field always started one. Patch changes the title and/or resizes using both dimensions. A WebSocket
client sends raw input as binary frames and receives raw output as binary
frames. JSON text control frames mark replay boundaries and acknowledge the
monotonic byte `cursor`; reconnecting with the last acknowledged cursor replays
the missing range from a bounded two-megabyte buffer; leaving `cursor` out is
the same as 0. A cursor older than the buffer gets `truncated: true` and a
replay that starts where the buffer does when that is a line start, else at
the first whole line still buffered (or, without a line break within 64 KiB,
the first character outside an escape sequence; a string such as an image or
a clipboard copy is skipped to its end), whose offset is `start`; `modes` then
holds the escape sequences that restore the terminal modes the dropped output
left on (alternate screen, application cursor keys and keypad, mouse tracking
and encoding, focus reports, bracketed paste, a hidden cursor, autowrap off,
insert mode). A client resets its terminal and applies `modes` before the
replay; clients that predate the field ignore it. Lifecycle changes also
publish `pty.created`, `pty.updated`, `pty.exited`, and `pty.deleted` SSE events.
Disconnecting a socket never terminates its process. DELETE does, as does
graceful server shutdown. A configuration hot reload (a provider connected,
settings, instructions or agents edited) does not: terminals are not part of
the runtime generation and keep their process, id and buffer. PTYs
intentionally do not survive server restart and their output is never
persisted in SQLite, so a client that would replace a server of another build
leaves it running while a terminal is open (see
[Desktop project model](DESKTOP-PROJECT-MODEL.md)). `pty.v1` advertises this
contract.

SSE envelopes include `schemaVersion`, `serverInstanceId`, monotonic `eventId`, type, relevant domain IDs, payload, and timestamp. A `turn.finished` payload is the finished `Turn` plus `background`: the background results its chat was still owed at that moment (the same count `/v1/threads/{id}/activity` reports), left out when 0 and never stored with the turn. A root turn that completed or failed with `background` > 0 is not where the chat stopped: the turn its last result starts is. Desktop, the terminal UI and the Cloud connector announce a chat as finished only then. `GET /v1/info` exposes `eventCursor`: snapshot-first clients capture it before REST hydration and connect with that value as `Last-Event-ID`, closing the snapshot/delta race. The stream sends a keepalive comment every 15 seconds, so a client may count one silent for longer (the SDKs' `idleTimeoutMs` / `idle_timeout`; Desktop and the TUI use 45 seconds) as a connection that died without closing, and reconnect from its cursor. Reconnects continue from the last applied event. A changed server instance or `resync_required` means clients must replace items, todos, the process-wide `/v1/turns/active` snapshot, pending requests, and thread snapshots from REST. Each connection takes its events from the server as they are published and keeps those its client has not read yet in a queue of its own, up to 16 MiB of event text besides the largest waiting event, so a client that reads more slowly than a model writes (a busy terminal UI, a phone through the relay) keeps its place, and one event larger than that (a message with several large screenshots) still goes out. One that falls further behind than that, or that the server's own buffer of recent events (the latest 2,048) dropped events for, is sent `resync_required` and its response ends: what it missed is gone, and it reconnects from its cursor as after any ended stream (the terminal UI does so quietly, as it is refreshing already). A replay that does not fit in the queue (a reconnect from a cursor with more than 16 MiB of events after it, or a connection without a cursor while the server's buffer holds that much) never ends the response: it is answered as a cursor older than the buffer is, with `resync_required`, and the stream goes on with the events published from then on. While they wait there, text and reasoning deltas (`turn.text_delta`, `turn.reasoning_delta`) of one turn join into one delta (up to 64 KiB of text) carrying the last joined event's `eventId`, so a slow client reads fewer, longer events, replaying after a reconnect included; other events are never joined or reordered, and a published chat's stream leaves out other chats' events before they could break a run. So `eventId`s increase but can skip. With several chats streaming at once on one connection their deltas alternate and few join, so such a client fills its queue sooner. The reply carries `X-Accel-Buffering: no`, so nginx and proxies that follow it pass events on as they are written, and `X-Content-Type-Options: nosniff` and a `connected` comment at once, so a client that buffers until it can sniff the type (Apple's URL loading) does not hold the first events back. SSE connections are observers: disconnecting, switching threads, or creating a new thread never cancels execution. Pending questions are root-scoped structured records (question, child thread, root thread, turn, choices, and creation time); `question.requested` adds one and `question.resolved` removes exactly that ID, so parallel child questions never overwrite each other. A question must be answerable: the `question` tool rejects a call that turns free-form answers off (`custom: false`) without offering a choice.

Runtime-source changes publish `configuration.reload_queued` when an active
turn delays application, `configuration.reloaded` after the complete new
generation is live, and `configuration.reload_failed` when validation rejects
the candidate. `assets.updated` follows a successful reload and tells clients
to refresh provider, command, agent, workflow, skill, MCP, and language-service
catalogs; with `resource: "provider"` it follows model lists that came in after
a catalog read stopped waiting for them (see [Model catalogs](MODELS.md)). The `serverInstanceId` does not change during a hot reload.
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

A background subagent the server stopped (a crash, `serve --stop`, an upgrade)
is not resumed, but the chat that launched it is told on the next start, once.
Nothing starts by itself after a restart, in any chat, scheduled ones
included: the result is recorded as a hidden user message the model reads
with the next message the person sends, `<task id="…" state="error">` saying
Haarness restarted before the task finished and that it can be continued with
its `task_id` once what it changed has been checked (`state="completed"` with
the result for a subagent that finished before its result was delivered).
Beside it an `error` item tells the person, in every app: “Haarness restarted
before the background task “…” finished. It was not resumed; your next
message tells the agent, which can continue it.” Both are written in the
same database step as the task's final checkpoint, so a crash or an error
during recovery cannot record one without the other. A chat that already
holds the result (its own checkpoint was lost, or a late one followed the
delivery) is not told again, and an archived chat is not told. A shutdown no
longer records such a task as `cancelled`: its row reads `interrupted` after
the restart. A result that ended before the shutdown but was still waiting
for its chat (the launching round had not finished) is recorded the same way
on the next start. Foreground subagents stop with the turn that waits for
them, which already says so.

A background result (a subagent's or a workflow run's) that its chat cannot
take as input is kept the same way instead of being lost: when the chat's
agent was removed or disabled meanwhile, or storage still fails after three
tries, the result becomes a hidden user message and an `error` item says
“The result of the background task “…” could not start a turn in this chat
(reason); your next message passes it to the agent.” (“workflow” for a
workflow's). Both are written in one database step while no turn can start;
if a turn started meanwhile, the result steers into it instead. A chat that
no longer exists, or is archived, is not told.

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
A prompt command's `user_message` item keeps the line it was typed as in
`command_line` (`/review src/foo.rs`), while its `text` becomes the prompt the
command expands to once it runs (`item.updated`), which is what the model
receives. Clients show `command_line` when it is present; messages saved
before it was kept, and ordinary messages, have none.

A `reasoning` item carries `duration_ms`, how long the model took over that
thought as the person waited for it, on a monotonic clock: from the moment
its request was sent (or from the text or tool call before it in the same
response) to the end of the reasoning. It therefore includes the wait for
the first reasoning token, and each request after a tool round starts its
own count. Clients read it as "Thought for 12s"; records made before it was
measured have none.

A `usage` item records one model request: `input_tokens`, `output_tokens`,
`cache_read_tokens`, `cache_write_tokens`, `cost_usd` (null when unpriced),
`estimated` when the provider reported no usage and the counts are Haarness's
estimate, and, for a request that finished cleanly after streaming something,
`first_token_ms` (from sending it to its first text, reasoning or tool call,
a tool call counting from when it starts streaming, not when its arguments
are whole), `stream_ms` (the span the output streamed across) and
`stream_tokens` (the tokens that streamed). A stopped or failed request
records none of the three, and a retry runs its own clock. Output speed is `stream_tokens /
stream_ms`, not `output_tokens`, which also counts reasoning that never
streamed. Desktop's Context panel and the TUI's `/usage` and sidebar show
them; records from before requests were timed have none.

MCP configuration accepts stdio, Streamable HTTP, and legacy SSE transports.
Remote entries may set `oauth`, `oauthClientId`, and `oauthScopes`; bearer and
refresh credentials never appear in the v1 wire configuration. A protected
server reports `needs_auth` with a non-secret detail until the CLI completes
the public OAuth authorization-code + PKCE flow. `disconnect` drops the live
worker and advertised tools without deleting configuration; `reconnect`
performs initialization and capability discovery again.
A local server, and a remote server's `headersHelper`, run in the server's
project folder (the chat home's own folder there); a relative `cwd` is read
from the project folder, `~/…` from the home folder, and the settings keep
`cwd` as written. Tools keep their own names in the API (`mcp__<server>__<tool>`
with the server's and tool's names as written); only requests to a model
provider carry names in the characters providers accept (see
[Plugins, skills and tools](REFERENCE.md#plugins-skills-and-tools)).
`timeoutMs` (default 30 s) bounds the socket connect, initialization, and tool
discovery as one deadline, plus a `headersHelper` run. In a JSON file a
`timeout` key counts too, in its ecosystem's unit: milliseconds in an opencode
entry (`type: "local"` or `"remote"`), seconds in a Claude Code or plugin
entry; edits leave the key and its unit as written. It does not bound tool
calls or the Streamable HTTP response streams that carry their results; a
timed-out connection names its limit ("MCP connection timed out after 30 s").
Tool descriptions are capped at 2048 characters, ending in `… [truncated]`;
set `HAARNESS_MAX_MCP_DESCRIPTION_LENGTH` to a positive character count to
change the budget. A tool schema without a root `type` gets `"object"`, as MCP
requires. Requests to Claude (Anthropic Messages, Bedrock, and Haarness Cloud
routers) omit any tool whose root input schema has `oneOf`, `anyOf`, or `allOf`,
or a root `type` other than `object`, which Claude rejects for the whole
request. A WARN line names each such tool once per process.
`apps: false` turns a server's interactive apps (MCP App pages) off: it is not
told that pages render, its results carry no `mcp_app` page and a read of one
of its `ui://` resources is refused (403); on is the default and is saved as
no key. A server's status (`GET /v1/mcp`) carries `apps: false` while they are
off and `appTools`, how many of its tools pair with a page, while it is
connected with them on; both are absent otherwise, so older clients see the
shape they knew.
Connected servers expose tool list/call, resource and resource-template lists,
resource reads, prompt lists, and prompt resolution through the corresponding
`/v1/mcp/{name}/...` routes. Unsupported prompt/resource capabilities return
an empty list rather than probing an undeclared server method.

`POST /turns`, `/input` and `/commands` also accept `resources`, the MCP
resources a person mentioned with `@`: `[{"server": "docs", "uri":
"docs://setup", "name": "setup"}]` (`name` optional, at most 50). The server
reads each from its MCP server when the request arrives, before the turn starts
or the message is queued, all of them within 10 s together, and adds each after
the text (after a command's string `arguments`) as a block under
`MCP resource: setup · docs:docs://setup`. Its text is fenced, and cut past
50 KiB (128 KiB for all of a message's resources) with a note that says where
the whole text was saved; its images and PDFs join `attachments` (10 MiB
together). A resource that is not read is named in its block with
`(not read)`, and the block holds only the reason: the MCP server's error, `did
not answer within 10 s`, or `a rule keeps this resource from the model`. The
chat's own rules decide as they do for `mcp_resource_read`, except that an ask
is not asked (the person picked it, as they attach a file): a deny for
`<server>:<uri>`, or a rule or tool setting that takes every tool of the server
away, keeps it out. The rest of the message is still sent. The same server and
URI twice is read once; two servers with one URI are two mentions. The rules are those of
the agent the message runs under: the request's `agent` (or its command's),
else the chat's. An `/input` sent while a turn runs may join that turn or run
as a turn of its own (a steer can arrive as the turn ends, a queued message
can be steered in), so both the running turn's agent and the chat's must
allow a mention; a deny from either keeps it out. Its rules are those at
arrival: if the chat's agent changes before a queued message runs, what was
read stays. No `PreToolUse` hook runs: a mention is no tool call. A message
with mentions is read and admitted on the server's own time, so a client that
goes away meanwhile does not lose it; a retry with the same `requestId` (a
turn, a queued message, a message steered into a running turn, or one that
started a turn, each recorded for this) finds it without reading or
delivering it again, even when retries race, and one sent while the first is
still reading waits for it. A published chat's request with `resources` is
refused (`403`), and its `/v1/info` leaves the capability out. Servers advertise
`mcp-resource-mentions.v1`; older servers ignore the field, so clients offer
resources only when it is there. `GET /v1/mcp?threadId=<id>&agent=<agent>` (either one alone works; a named
agent wins over the chat's) marks each server those rules take away with
`turnedOff: true`, so an `@` list can leave its resources out. While a turn
runs in that chat, it also marks those the running turn's agent or the
chat's takes away, as a message sent then follows both.
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

Images are judged by their bytes, never by file name or declared type, and made
into images every provider reads before the message is stored: a JPEG named or
labelled PNG is sent as JPEG; BMP and TIFF become PNG; HEIC/HEIF photos become
JPEG on macOS (through the system's `sips`) and are refused elsewhere with a
request to export them as JPEG or PNG; images larger than 2000 pixels on a side
or 3.75 MB (Anthropic's 5 MB base64 limit on Bedrock and Vertex) are shrunk,
first keeping PNG, then as JPEG. A converted image is stored inline as a data
URL; an image that already fits keeps its path. Bytes that do not decode are
refused with `400` and the reason; a PNG whose chunk checksums are wrong, which
browsers still show, is corrected instead. The `read` tool and saved browser
screenshots apply the same rules; a `read` of a damaged image fails the call
with the reason.

An attachment or tool image whose file is deleted later is named in the message
("no longer available") instead of failing every later request.

A long chat can hold more media than a provider takes in one request: many
screenshots, or a few large PDFs. Before each request the runtime counts what it
would send, as the base64 the provider receives, against two limits. The images
alone are held to 25 MiB (Anthropic and Haarness Cloud take 32 MB in a request;
OpenAI, the Gemini API and Gemini on Vertex AI take more), 15 MiB on Bedrock,
and for Claude models, directly, on Bedrock or through Haarness Cloud, also to
100 images in a request to a model with a 200K-token context window and 600 to a
larger one; only images are replaced for these. Where the provider states what
it takes in one request (Anthropic 32 MB, Bedrock 20 MB, Haarness Cloud 32 MiB,
and 32 MB for Anthropic's models through it), every attachment (images, PDFs,
audio and video) and the text of the messages together are held to that too,
less 1 MiB for the system prompt and tools. That measure is close, not exact:
the text after the last attachment, JSON escaping, and the system prompt and
tools are estimated by that 1 MiB rather than counted. Other providers (OpenAI,
the Gemini API, Gemini on Vertex AI and OpenAI-compatible servers, which take
more or do not say) get no such cut; a refusal of the request's size is
compacted for instead. Where a message takes a total past its limit, the
attachments before it are replaced by a one-line note, oldest first, until the
rest fit in three fifths of the limit (the person's own attachments in that
message's turn stay, and what cannot be reached that way stays too). The
attachments of the message that passed the limit go only when they and the
person's together still pass the limit itself, oldest first and only until the
request fits, so the newest attachment reaches the model whenever it fits beside
the person's. For the whole request, a PDF, audio or video file of the turn
that passed it is never replaced, whoever added it, or the note would send the
model to read again what it just read; a request too large all the same is
refused, and compaction answers that. The note names the attachment by its kind
and name (a file by its path), says it is no longer visible (audible, for audio)
and must not be described from memory, and says to read the file again, or for
an attachment the person sent to ask them to attach it again, if it is needed.
The cut moves only when the attachments kept pass a limit again, not with each
new one, so the requests between two cuts carry the same history and the
provider's prompt cache holds. The person's own attachments are those they
attached to their messages (steers included, not what Haarness adds or a tool
read); when those alone pass the limit the request is sent as it is. Which turn
an attachment belongs to is read from the history, not from the turn now
running, so a new turn's first request repeats every earlier cut and reads the
cache too. Stored messages do not change, and the transcript shows nothing: a
cut is logged as one `INFO` line of counts.

When a provider still refuses an image (its type, size, pixels or count, or a
request too large because of images), the runtime appends a `media_omitted`
item to the thread with the SHA-256 `fingerprints` of the refused attachment
sources, their display `names`, the provider's `reason`, `guessed`, `oldest`,
`too_large` and `too_many`. Every later request of that thread, after restarts
and in forks, sends a short note in place of those images, so the prompt stays
stable for caching; the original attachments stay in their messages. When the
provider names the image by its place in the request (Anthropic's
`messages.N.content.M`) only that image is left out. A refusal of the request
for how many images it carries ("too many images", recorded with
`too_many: true`) or how large they make it (HTTP 413, Gemini's "Request
payload size exceeds the limit", recorded with `too_large: true` whichever
images it leaves out) leaves out the oldest half of them by that measure
(`oldest: true`), so the newest images then reach the model. The turn's own
images and those of the last message carrying any stay, and so does a file read
again among them: the record leaves a source out everywhere. When those that
stay hold more than half of the refused request by that measure, leaving out
the others would not help, so the newest go instead and the older ones are
kept. Any other refusal that names no image leaves out the newest images
(`guessed: true`): what the last request added is the likeliest cause. A
refusal before anything streamed is retried once per turn; a second refusal in
the same turn fails it, and the next turn sends without the images. TUI and
Desktop show the item as a notice.

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

## Capabilities

`GET /v1/info` lists `capabilities`, the contract a client can rely on. A
client checks a versioned name before it offers what depends on it, and
treats a missing one as an older server.

| Capability | What it promises |
| --- | --- |
| `approval-mode.v1` | The server keeps each chat's Auto or Review mode (`/approval-mode`) and answers eligible requests itself; clients never reply automatically |
| `approval-mode.yolo.v1` | The server also keeps a chat in Yolo (`"mode": "yolo"`), approving every request that would otherwise ask, once; clients offer Yolo only where this is listed |
| `approval-review.v1` | Auto asks a reviewer model (`auto_review.enabled`, on by default): a request it leaves to a person carries `review` |
| `btw.v1`, `btw.stream.v1` | Side questions; the second adds the side chat's earlier questions and the streamed `/btw/stream` |
| `configuration-hot-reload.v1` | Configuration edits apply without a restart, with the reload events above |
| `input-queue.v1` | `delivery: "queue"` and the revisioned `/input/queue` routes |
| `lsp.v1`, `formatter.v1` | `GET /v1/lsp` and `GET /v1/formatter` |
| `mcp-resource-mentions.v1` | `resources` on `/turns`, `/input` and `/commands` (left out for a published chat) |
| `projects.v1`, `workspaces.v1`, `workspace-cleanup.v1` | The project and worktree routes, and the separate stale-registration `/cleanup` |
| `providers.refresh.v1` | `POST /v1/providers/refresh` |
| `pty.v1` | Terminals (`/v1/ptys`); `ifNone` and `created` came later, so a `pty.v1` server may ignore the one and leave out the other |
| `remote-chat-scope.v1` | The server confines a published chat's requests to its chat tree; the Cloud connector does not serve through a server without it |
| `session-archive.v1` | Portable session archives |
| `thread-create-id.v1` | A client-chosen `id` on `POST /v1/threads` |
| `workspace-history.v1` | Git review and undo/redo; the CLI and TUI use a running folder server only when it has this and `pty.v1` |
| `browser.v1`, `browser-drafts.v1` | The browser host routes (servers with a browser broker) |
| `browser-extension.v1` | Haarness for Chrome: the extension host route and its settings |
| `input-settings.v1`, `browser-settings.v1`, `usage-statistics.v1` | The management resources of those names |

`rest`, `sse`, `network.attach`, `attachments.inline-upload`, `files.base64-write.v1`,
`files.preview.v1`, `mcp`, `skills`, `subagents`, `workflows`, `workflows.read`,
`goals`, `ultracode`, `opentui`, `tool-catalog.v1` and `lazy-tools.v1` name
features every current server has; no client in this repository branches on them.

## Interactive configuration and sharing

`POST /v1/manage/{resource}` accepts an authenticated JSON action and returns a
`ManagementView` (`title`, `lines`, `fields`, `entries`, optional `submit`
and `data`). The form resources are `providers`, `mcp`, `schedules`, `hooks`,
`memory`, `profiles`, `sharing`, `remote-control` (see
[remote access](REMOTE-ACCESS.md)), `marketplace-ui` and `capabilities-ui`.
Begin with `{"action":"view"}`; `sharing` and `remote-control` also require
`threadId`. `input-settings`, `browser-settings` and `usage-statistics` answer
a view whose `data` holds their settings (advertised as `input-settings.v1`,
`browser-settings.v1` and `usage-statistics.v1`), and `capabilities` and
`marketplace` answer plain JSON (the SDKs' `capabilities()` and
`marketplace()`). Render fields, preserve the returned submit/revision metadata, merge
edited values into that submit object, and send it to the same resource. An
entry's `confirm` text requires explicit user confirmation before its request is
submitted. `secret` fields must use masked input and never be copied to logs.

TypeScript exposes `manageResource(resource, body)` (and `managementView()`,
typed as a `ManagementView`); Python exposes `manage_resource(resource, body)`. These are user-management APIs, separate from
model tool execution. Errors leave the client draft intact.

`POST /v1/manage/assets` serves the structured definition editors. Send `kind`
(`agent`, `command`, or `skill`) and `action` (`list`, `get`, `create`, `edit`,
`customize`, or `delete`; a skill also takes `import` with the `path` of a
skill folder or `SKILL.md`). It returns definition rows/details or a mutation result,
rather than a `ManagementView`. Writes use the connected server's configuration;
`scope` is `global` or `local`. A listed row from the
[system folder](CONFIGURATION-SCOPES.md#system-folder) has `scope: "system"` and is
read-only; sent back as a request's scope, `system` reads as no scope. `customize` creates a same-name override from a
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
