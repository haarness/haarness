# Projects, worktrees, and conversations

**Project** is the sidebar group. **Project folder** is its main checkout. A **Worktree** is an additional Git checkout belonging to that project. An ordinary non-Git folder is also a project. “Workspace” remains an internal API/storage term for a concrete working directory; it is not another desktop navigation concept.

## Identity and execution

The native project registry determines ownership through Git's common directory and primary checkout. The desktop does not infer relationships from path prefixes. A worktree outside the repository therefore joins the same project; a subdirectory of the main checkout is not mistaken for a linked worktree. Remote endpoints remain separate from local projects, even if their paths match.

`src/projects.ts` projects the native registry into desktop groups. Each location retains its original connection ID, actual path, server, session database, and runtime lease. Conversations are identified in presentation state by both location and thread ID, so imported IDs cannot collide across checkouts. Opening a conversation routes transcript, tools, permissions, files, review, and terminals to that location.

Opening only a linked worktree also registers the primary project folder. Registered, existing worktrees appear in the project's working-location menu. Unavailable worktrees are hidden from the sidebar and working-location menu, retaining their identity and cached history. In worktree view, the project menu offers **Show unavailable worktrees** with a count, so users can inspect or remove stale registrations. This temporary reveal resets on reload; **Hide unavailable worktrees** dismisses it immediately. Restoring the folder at its original path and refreshing the project returns the worktree and its conversations to the normal list.

## Navigation and drafts

- Clicking another project's title activates it, restoring its last working location and conversation, and shows its conversation list; clicking the current project's title shows or hides its list. The chevron independently expands/collapses its conversation list. Selecting a project never reorders projects or collapses other expanded projects.
- The project menu offers **Pin**, **Rename…** (a custom display name), **Section**, **Reveal in Finder**, **Archive chats** and **Remove project**. Sections list first, then unfiled projects under a **No section** heading once any section exists; a pin keeps a project at the top of its own group. Each group orders its rows through the shared persistent `list:projects` ordering, so Alt+arrows and drops move among the group's own rows, and a drop onto another group's row files the project there and places it beside that row. A custom name replaces the registry name wherever the desktop shows the project. Pin, name and section live in desktop storage (`project-presentation:v1`) keyed by project ID, and they follow a bare folder when it later resolves to its registered project. Archiving a project's chats archives every independent conversation in its available checkouts through the normal thread API and leaves the current chat open.
- The project menu offers **Enable worktree view** for local Git projects. It replaces the flat list with collapsible **Local** and worktree groups, each with its own sessions, branch, new-chat button, and checkout menu. The preference and expansion states survive restarts. Creation, reset, removal, and refresh live in this sidebar; there is no separate Worktrees page. Non-Git folders retain the flat list.
- In flat view, New chat defaults to **Project folder**. In worktree view it keeps the current checkout; each group also starts chats directly in its own folder. The composer still offers existing and new worktrees. Selecting a location, model, agent, or starter does not create a conversation.
- Flat-view new-chat drafts belong to the project, so choosing a worktree preserves text and attachments. Worktree-view new-chat drafts belong to their concrete checkout. Switching views preserves each saved draft. Sending creates a conversation bound to the chosen folder. Existing-conversation drafts are always keyed by location and thread identity.
- In flat view, independent conversations from all locations appear together; in worktree view they appear beneath their owning checkout. Delegated agent conversations are reached through the parent transcript; forks remain in the sidebar. `parentId` records lineage, while `isSubagent` distinguishes actual delegation. Worktree conversations have a small checkout badge; the title bar also identifies the active worktree.
- Projects, conversations, and tabs drag from their whole row, without dedicated grips, move menus, or success notifications. Alt+arrow keys and Alt+Home/End provide keyboard ordering while preserving focus. Sidebar conversations have one Archive/Restore action; renaming and forking remain in the conversation header. Archiving an inactive project's conversation does not navigate away from the current chat.
- Open chat tabs are remembered per location in desktop storage (`chat-tabs:v1:<location>`), beside its selected chat (`session:<location>`), and reopen when the location is activated again; tabs of missing chats, and of chats the reader archived, are dropped (an agent's chat that the server archived when its work ended keeps its tab). The preview tab, which a plain click opens and the next one replaces, is left out of that list; when it was the selected chat it comes back through `session:<location>`, as the preview again.
- Projects, conversations, and tabs use the existing persistent ordering system. A versioned migration folds old location-level project order/expansion into project groups and qualifies saved session positions with their locations.
- Each expanded project maintains lightweight indexes for its locations. Versioned, paged title caches preserve all summaries during startup/recovery without a conversation-count cap. These are navigation caches, not a second transcript database. Collapsing releases subscriptions without stopping tasks.
- Permission/question state belongs to its task and location. A waiting task marks its sidebar row while other chats, projects, settings, and menus remain usable.

Configuration, credentials, plugins, skills, and executable resources continue to use the shared [global/project scope model](CONFIGURATION-SCOPES.md). Desktop appearance/layout state belongs in desktop storage; agent configuration does not.

## Runtime lifecycle

Local servers start automatically in the background. Opening a directory and saving its settings form one transaction, serialized only against other opens of that same directory. Background refreshes read the latest committed setting, so they cannot restore an older trust choice during a restart. Filesystem leases coordinate independent desktop/CLI clients. A slow project's launch cannot block another project's navigation or replace a more recent selection.

Reset and removal execute through the owning project folder. The desktop serializes them against opens of that checkout, refuses active turns, pending permissions/questions, running or paused workflows, and running terminals, and closes an idle server before the backend acquires its lease. Dirty checkout protection remains in the backend; discarding tracked edits or both tracked and untracked files are separate explicit reset choices. Reset retains ignored files. Removal leaves conversation data intact and removes only the checkout registration from desktop navigation.

A rebuild or an upgrade can change the executable while an earlier daemon is busy: it runs a turn, holds a terminal open, waits on an approval or question, or runs (or holds paused) a workflow. Desktop, the TUI and CLI commands reuse that daemon when authenticated identity, directory, capabilities, and trust are compatible, preserving its work, pending approvals and terminals; the TUI and CLI say so on stderr, naming what keeps it busy (for example `1 open terminal (npm run dev)`). Desktop's and the TUI's own installs count as different builds, so opening a project in one while the other has a terminal open there keeps that terminal. The first client to find the daemon idle replaces it. Incompatible protocols and explicit trust changes retain their lifecycle guards: a busy daemon is then refused with the same list, and a client never stops it. Closing Electron detaches clients; it does not stop tasks or answer permissions.

## Practical scale

Automatic snapshots now default to Git projects. A plain folder such as Downloads is not scanned for automatic checkpoints unless snapshots are explicitly enabled. A chat without a project is the exception: its own folder is checkpointed by default, that folder alone, so Undo works there although the chat home is no Git repository (`snapshots.enabled = false` turns it off). Snapshot capture runs off the asynchronous server executor and records the complete managed manifest, without the former 10,000-entry rejection. Ignore rules, symlink handling, restore validation, and SQLite atomicity remain intact.

The explorer no longer truncates directories at 2,000 entries. Extension catalogs have no 64-tool cap; terminals have no 20-session cap; profile archives have no 4,096-file cap; memory search no longer ignores entries after the first 1,000. Profile export deduplicates identical migrated resources while preserving distinct legacy content.

Byte budgets still bound previews, diff output, IPC payloads, and archive expansion. For example, large snapshot file content is represented as omitted metadata, rather than rejecting the whole project. A path snapshots cannot read (a root-owned directory, a file still changing when read twice) is likewise recorded as omitted, with one warning per path, instead of failing every edit; undo never writes or deletes it and refuses when such a path changed between its snapshots. These budgets are distinct from arbitrary project-size or item-count limits.

## Reference implementations and compatibility

Haarness uses separate ownership for project navigation, draft state, and directory execution.

The preferred CLI/TUI management command is `worktree` / `/worktree`. Existing `workspace`, `workspaces`, and API `/v1/workspaces` remain compatible aliases/contracts; database and runtime directory names are unchanged. This naming change does not migrate or copy live conversation databases.

See [desktop verification](records/2026-09-07-desktop-verification.md) for executed scenarios and evidence limits.

## Chats without a project

On this computer a chat needs no project. The sidebar's **This computer**
section, above Projects, lists these chats; its **+** starts another. With
none yet it is the heading and its +. A first launch, or this computer with no
project remembered, opens on a ready message box whose folder chip says **No
folder**; no folder chooser opens. It shows at once, also while the chat
home's daemon starts (no loading screen): the model pill says it is loading,
and a message sent meanwhile waits for the chat home, its send button busy,
and then goes. The top **New chat** starts where you are:
in the open chat's project, or without a folder when This computer's chat (or
nothing) is open. A project's own New chat starts in that project.

Each such chat works in a folder of its own that Haarness makes when the chat
is created: `<Desktop userData>/Chat folders/<YYYY-MM-DD>/<name>`, named from
up to six words of the first message folded to lowercase ASCII (40 characters
at most; `chat-` and four letters when the message gives no name), with `-2`,
`-3`… when the name is taken. It is the chat's working directory and its
boundary, as a project folder is for a project's chats: tools, shell commands,
plans and Undo's snapshots work there, and a path outside it, a sibling chat's
folder included, is outside (see [A chat's own
folder](PERMISSIONS-AND-EXECUTION.md#a-chats-own-folder)). Delegated chats,
forks and side chats work in their parent's folder. Archiving a chat keeps its
folder.

All of these folders live in one **chat home**, `Chat folders` in Desktop's
app data (never Documents, which a cloud drive may sync; never `Chats`, which
on a case-insensitive disk is the older general-chat folder). One daemon
serves it: Desktop starts it with
`haarness --workspace "<userData>/Chat folders" --chat-home desktop`, which
marks the folder as [the chat home](REFERENCE.md#the-chat-home) and keeps
project configuration off (no ancestor `.haarness`, `AGENTS.md` or repository
approvals). It is saved once
among the desktop's locations. Its daemon starts only when This computer is
used: its new chat or one of its chats is shown, its + or No folder is chosen,
or Search is open; until then the sidebar lists its chats from the cache of
the last session, and once used they are followed live, running work
included. It is never a
project: Settings → Projects, the project lists of Search and the folder chip,
`desktop projects`, the host catalog and other computers leave it out. Only
this computer has it; remote servers and other computers still open a project
first, from their Home.

The message box's bottom row carries the folder chip. For a new chat it opens
a searchable list: **No folder**, the projects in sidebar order, and **Open
folder…** (the system folder chooser). For a chat that has started it names
the folder the chat works in, the chat's own folder or its project, and offers
Reveal in Finder; a chat never moves. The new-chat heading reads "What should
we work on?" without a project and "…in {project}?" with one. For a chat
without a project the Files panel shows its own folder, a new terminal opens
there (each chat has its own terminals), `@` and Search find the files in it,
a reply's relative image or file link (`![](chart.png)`) starts there and may
not leave it (every file read and write asks the server for that chat's folder,
which names files from it and follows no link out of it), `/` offers the workflows in its `.haarness/workflows` (and no
other chat's), and Review treats it as a folder without Git. Before its first message there
is no folder yet, and those panels say so. Search lists these chats with
**This computer** and the chat's folder.

The legacy `projectless` marker is removed from desktop storage at startup,
and the IPC connection API still rejects it. Older general-chat records
migrated to ordinary projects in their existing working folders, with their
connection IDs, conversation databases, saved drafts, active session and trust
settings intact; existing `<desktop userData>/chats` folders stay in the
Projects list as such.

Startup and server switching restore an explicitly remembered project (This
computer counts as one), never the first saved project. An unavailable
project offers Retry and Choose another project. Opening a project or an
existing conversation shows a centered Haarness mark until the selected
history is loaded, without flashing the new-chat screen or Home. A newer chat
selection supersedes an earlier pending history request. The loading
animation respects reduced-motion preferences.

## Remote hosts with multiple projects

An authenticated host exposes `/v1/host/projects`, `/v1/host/directories?path=...`, and `POST /v1/host/open` with `{ "path": "/absolute/folder" }`. The catalog uses `WorkspaceListResponse`; opening returns the canonical registered `WorkspaceInfo`. Listing looks at every registered folder at once for whether it still exists and its branch (read from the repository's `HEAD` file), and waits at most a second: a folder that has not answered by then, such as one on a network volume that went away or behind a privacy prompt nobody is there to answer, shows as last recorded, or as it answered a later look, and is not looked at again until that look returns. The same applies to `GET /v1/workspaces` and the project lists of the TUI and Desktop; Desktop's list also registers each sidebar folder (and the project of a linked checkout) the same way, so a sidebar folder that has not answered within a second is left as registered before instead of holding the list; a detached `haarness desktop` process registers it however long it takes, one such process per folder at a time. Registering and opening a folder run git before taking the registry lock, so a folder whose git stalls never holds up the registry for anything else. The `.git` pointer and `HEAD` are read only when they are regular files of at most 4 KB, so a `.git` linked to a device or a FIFO cannot block or fill a listing. Directory listings contain canonical `path`, optional `parent`, `directories` (`name`, `path`), and optional `nextCursor`. The folder explorer uses this endpoint alone, without fetching the project catalog. Browsing is read-only; opening an existing directory registers it and ensures its normal daemon. Invalid/unavailable paths return an error without changing the desktop selection. `DELETE /v1/host/projects/{workspaceId}` closes a project for remote access (it leaves the catalog and its routes refuse until the folder is opened again); despite the path, it takes the workspace id (`workspaces[].id`), like the routes below.

`/v1/host/workspaces/{workspaceId}/v1/...` routes directory APIs to that daemon, including event streams and terminal upgrades. A daemon another Haarness build started is replaced when it is idle; while it is busy (a turn, an open terminal, a pending approval or question, a running or paused workflow) it is left alone, plain `GET`/`HEAD` requests still reach it (when it speaks this API version and enforces published-chat scope) so the project can be viewed, and everything else is answered `409` `{"error":{"code":"project_busy","message":"This project is busy on a different version of Haarness. Open it on the computer, or wait until its work finishes.","retryable":true}}`. A daemon of this build that runs with the other project configuration setting is handled the same way, with the message "This project is busy with a different project configuration setting. Open it on the computer, or wait until its work finishes.", and a daemon that does not say within 750 ms whether it is busy counts as busy. Unlike the TUI and CLI, requests through Cloud never use such a daemon as it is. Other host failures are `400` `remote_project`. Every gateway request uses the host’s configured authentication (optional username/password, or a legacy saved bearer credential) and normal Origin validation. The host resolves registry IDs, never client-supplied upstream URLs, and replaces authorization internally. Streams are detached when the gateway shuts down; other directory daemons keep their own work.

New remote connections are keyed by server origin plus opaque workspace ID. The existing origin-only default connection is retained when reopening its canonical folder. Server identity, credentials and last-project state remain separate from each location's drafts, event cursor and conversation state. Removing one desktop project preserves the host connection and server-side history.

Your other computers, opened through Haarness Cloud's relay ([Desktop](DESKTOP.md#servers-and-projects)), use the same host API under `/api/v1/remote-hosts/{hostId}/runtime`. Their server is `host:{hostId}` and each folder is keyed `host:{hostId}/workspaces/{workspaceId}`, whichever Cloud address relays it; the saved location keeps the Cloud address it came from, and the folder's project from the host's catalog as `host-project:{hostId}:{projectId}`, so linked checkouts of one repository group as they do locally. No credential is saved: Electron main asks the account for the current sign-in when it needs one.

Opening and routing inspect only the selected workspace. Startup reads registry metadata for migration and snapshot policy without probing every saved folder. The full catalog refresh is reserved for listing projects. Desktop reuses the connection's verified server information and loads chat state and model choices concurrently; it becomes ready only after both complete. Opening failures preserve the path and distinguish slow servers, access problems and missing folders in plain language.


### Remote folder explorer

The remote picker is a folder explorer with Back, Up and Home navigation, a path bar with debounced autocomplete, and a hidden-folders toggle. Click a folder to browse it, then choose **Open project**. In the path bar, Up/Down selects a suggestion, Tab completes it, Enter navigates, and Escape dismisses suggestions. Pending older responses cannot replace a newer path or selection.

`GET /v1/host/directories` accepts:

| Query | Behavior |
| --- | --- |
| `path` | Absolute server path; omitted starts at the served project. `~` and `~/...` resolve on the server. |
| `prefix` | Case-insensitive folder-name prefix for autocomplete. |
| `hidden` | Include dot folders; false by default. A prefix beginning with `.` also matches hidden names. |
| `limit` | Page size from 1 to 200; default 200. |
| `after` | The previous response's `nextCursor`; preserve the path and filter when requesting another page. |

Results sort by lowercase name, then exact name, and exclude ordinary files. The server retains at most one page plus a look-ahead entry in memory while scanning. `nextCursor` is null when there are no more results. Symlink directories are browsable; navigating resolves the canonical path. Unavailable or unreadable parent folders return an error, and the picker keeps the entered path for correction. All browsing uses the selected host's authentication and Origin checks. No client filesystem listing is substituted for the remote host.
