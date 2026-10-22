# Haarness reference

The long-form reference for the `haarness` executable, the server and its clients: every command family,
how the server and clients share work, worktrees, the terminal UI, agents and workflows, schedules,
plugins, providers and the full configuration. The [README](../README.md) is the short tour; the user
guides at <https://haarness.ai/docs> teach the same ground step by step.

## Install options

`haarness upgrade` installs a newer release. Installers verify SHA-256 before replacement, and
macOS curl installations clear quarantine on the verified payload, including nested Desktop
executables. Use `HAARNESS_CHANNEL=preview` for preview builds and `HAARNESS_VERSION` to pin
one. See the [release testing skill](../skills/haarness-release/SKILL.md) for local installation,
publishing, platform validation and rollback.

## What works

- [Haarness for Chrome](CHROME-EXTENSION.md): agents in the TUI, CLI, Desktop and SDKs
  can use your own Chrome profile, signed in as you, alongside Desktop's isolated browser.
  Agents work only in their own tab groups, every site asks per chat (scripts separately), and
  the Haarness button in Chrome pauses, stops or answers. Set up with `haarness browser setup`,
  `/chrome` or Settings → Browser → Chrome; see [Browser alongside chat](BROWSER.md).

- [Haarness Router integration](ROUTER.md): connect a self-hosted router by
  address in Desktop, TUI, or CLI. Access keys are optional, models refresh
  automatically, and multiple named connections are supported.

- Bundled ripgrep (`rg`) works offline in agent shell calls and integrated
  terminals, with no separate installation. `haarness doctor` verifies the
  embedded executable. See [bundling details](../vendor/ripgrep/README.md).

- Unpublished [TypeScript and Python SDKs](../sdks/README.md) cover the complete
  server API, including sync/async Python, events, PTYs, and owned local servers.
  Run `./scripts/test-sdks.sh` for generation, typing, packaging, and real-server
  end-to-end coverage of every SDK operation.

- Optional TypeScript/JavaScript runtime extensions: local or npm installation,
  typed custom tools and chat/lifecycle hooks, explicit trust, process isolation,
  cancellation, and idle-time reload. Start with `haarness extension init ./my-extension`.
  See [Extensions](EXTENSIONS.md).

- Portable configuration profiles: `haarness profile save/inspect/restore`,
  global/project scopes, and authenticated cloud upload, discovery, download,
  restore, and deletion through a self-hosted server. See [Profiles](PROFILES.md).

- Cross-project analytics: `haarness stats --days 7` shows project/model/tool usage;
  `haarness insights --days 7` generates a private HTML report with findings.
  The TUI exposes `/stats` and `/insights`. See [Insights](INSIGHTS.md).

- Usage statistics, on by default: a daily summary of chats, turns, tokens, models and
  tool calls (never prompts, code, file names or paths) shared with Haarness Cloud.
  `haarness telemetry preview` shows exactly what is sent;
  `haarness telemetry off`, `HAARNESS_TELEMETRY=0` or `DO_NOT_TRACK=1` turns it off.
  See [Usage statistics](USAGE-STATISTICS.md).

- Durable UUIDv7 Thread → Turn → Item history in SQLite, idempotent turn requests, ordered events, bounded SSE replay, interruption, compaction records, forking, and restart recovery without tool replay.
- Agent Client Protocol v1 over stdio for editor integrations, including durable create/load, streamed text and reasoning, tool lifecycle and approvals, plans, PDF/image/audio/video context, cancellation, and stdio/HTTP/SSE MCP handoff.
- Persistent goals with runtime-owned token/time accounting, bounded autonomous continuations, negotiated evidence-backed completion criteria, strict blocked/complete exits, durable lifecycle events, and localized completion actions.
- Dynamic workflows with static project/global/plugin discovery, native and restricted-portable runtimes, permission-checked agents/tools/shell/nesting, immutable source snapshots, durable logs and child sessions, lifecycle controls, live events, and TUI/CLI/model-facing management.
- Durable native scheduling with CLI and model-facing management, overlap locks, run history, and OS registration through launchd on macOS, systemd user timers on Linux, and Task Scheduler on Windows.
- Native OpenAI Responses, Anthropic Messages, Gemini, Azure OpenAI, Google Vertex AI, GitHub Copilot, Amazon Bedrock, and OpenAI-compatible adapters with authenticated model discovery, provider-specific authentication and tool-schema transforms, required schema-constrained workflow output, durable Gemini thought signatures, PDF/image input across native adapters, Gemini audio/video understanding, and provider-neutral text, reasoning, tools, usage, cancellation, and error events.
- Built-in read/list/glob/grep/write/edit/multiedit/apply-patch/bash/web-fetch tools, PDF/image/audio/video-aware file reads, optional image/video generation, permission-preserving batch calls, confined JavaScript orchestration over MCP tools, exact ordered permissions, blocking approvals/questions, durable child agents, plan/review/explore profiles, and configurable child-agent concurrency (four at a time by default).
- MCP stdio, Streamable HTTP, and legacy SSE initialization; generic OAuth 2.1/PKCE discovery; durable authorization state; explicit connection status; capability discovery; namespaced tools; and direct calls through the official Rust SDK.
- Recursive Agent Skill discovery and loading from global, project, and Haarness plugin roots, plus CLI create/get/list/remove lifecycle and `AGENTS.md` project context.
- Recursive slash-command and custom-agent discovery with project/global CLI lifecycle, collision-safe plugin namespaces, command model/agent overrides, and custom-agent model pinning for primary and child sessions.
- Opt-in native OpenTelemetry tracing with batched OTLP/HTTP protobuf export, W3C inbound trace-context propagation, configurable sampling/resources, and command, HTTP, agent-turn, provider-request, and tool-call spans.
- A quiet editorial OpenTUI interface built from edges rather than boxes: a launch masthead that becomes scrollback, a prompt that floats on the page over a rule that breathes while a turn runs, a command-driven custom status line, 17 selectable UI languages shared with Desktop (English by default), compact activity rows, PDF/image/audio/video drag-and-drop chips, filtered slash/file autocomplete, borderless command/thread/model/agent selectors, reasoning controls, permission prompts, persistent tabbed workspace terminals, and reliable terminal cleanup.
- Owner-only credential and runtime-discovery files, loopback-by-default binding, explicit authenticated network exposure, host/origin validation, and redacted-by-construction normal logs.

## Build from source

The repository vendors OpenTUI v0.5.9's native source and checksummed libraries for every supported target, then embeds the selected archive. Normal builds are offline and end users do not need Bun, Node.js, TypeScript, Zig, or OpenTUI. Terminal capabilities are forwarded across the Rust/native boundary; terminals without truecolor use OpenTUI's palette-aware ANSI-256 renderer, including Apple Terminal when `COLORTERM` incorrectly claims truecolor support. Haarness queries the terminal's default background to select a readable dark/light variant. In the default `auto` background mode it preserves that background when exact RGB page colors are unavailable, unless you explicitly choose or preview a theme. Explicit choices paint their own background, including light themes on dark terminals, and survive restart. Page, sidebar, transcript, code, and status surfaces then use the terminal background together; menus, dialogs, selections, and emphasized diffs remain solid. Set `[tui] background_mode = "theme"` or `"terminal"` in `~/.config/haarness/config.toml` to force either behavior.

The default `haarness` theme pairs warm charcoal with paper-white focus in dark mode, and warm paper with ink focus in light mode, matching Desktop. Use `/themes haarness dark` or `/themes haarness light`, or open `/themes` to preview the collection. Existing saved theme choices are preserved. Every built-in light palette has light surfaces and contrast-checked body, secondary, syntax, and diff text.

```sh
cargo build --release
./target/release/haarness doctor
./target/release/haarness
```

## Command examples

The full list is in [Find the right command](https://haarness.ai/docs/cli/overview) and in `haarness --help`.

```sh
haarness serve --port 4096
haarness serve --daemon              # start/reuse the workspace server
haarness serve --stop                # explicit graceful server shutdown
haarness acp --provider openai --model gpt-5.6 # ACP agent for editors
haarness tui --attach http://127.0.0.1:4096 # no credentials needed by default
HAARNESS_SERVER_PASSWORD="$PASSWORD" haarness serve --host 0.0.0.0 --port 4096
haarness attach http://server.example:4096 --username haarness --password "$PASSWORD"
haarness attach http://server.example:4096 --continue --fork --password "$PASSWORD"
haarness run --attach http://server.example:4096 --password "$PASSWORD" \
  --provider openai --model gpt-5.6 "Inspect this workspace"
haarness                               # first run: the connect picker is the welcome screen
haarness auth login openai             # ChatGPT Plus/Pro browser OAuth (same flow as /connect)
haarness auth login openai --method headless
haarness auth login xai                # SuperGrok device OAuth
haarness auth set openai                # asks for the key; a piped key works too
haarness auth logout openai
haarness run "Inspect this repository"  # your default model, else a connected provider's
haarness "Inspect this repository" -m openai/gpt-5.6-sol
haarness run --provider openai --model gpt-5.6 "Inspect this repository"
git diff | haarness run "Review this change"  # piped stdin is appended to the prompt; an idle pipe is ignored when a prompt is given
haarness run --goal --provider openai --model gpt-5.6 "Ship and verify the migration"
haarness run --ultracode --provider openai --model gpt-5.6 "Audit every API boundary"
haarness run --provider gemini --model gemini-3.5-flash --file ./brief.pdf --file ./clip.mp4 "Compare these"
haarness providers
haarness agents
haarness agent create reviewer --description 'Review changes' --model anthropic/claude-sonnet-5-5 --tool bash=false --permission 'edit:*=deny' --prompt 'Review for correctness.' --project
haarness command create audit --description 'Audit a subsystem' --template 'Audit $ARGUMENTS' --agent reviewer --project
haarness skill create release-check --description 'Check release readiness' --instructions 'Verify build, tests, and artifacts.' --project
haarness sessions list
haarness sessions export SESSION_ID --output session.json
haarness sessions export SESSION_ID --sanitize --output shareable-session.json
haarness sessions import session.json
haarness worktree list
haarness worktree create review-fix --from main
haarness worktree open review-fix
# destructive operations require an explicit acknowledgement; dirty files
# need the additional discard flags shown by `--help`
haarness worktree reset review-fix --confirm
haarness worktree remove review-fix --confirm --delete-branch
haarness mcp ls
haarness mcp add files --command my-mcp-server --arg=--root --arg=. --env TOKEN='${TOKEN}'
haarness mcp add remote --url https://example.test/mcp --header 'Authorization=Bearer ${TOKEN}'
haarness mcp add protected --url https://example.test/mcp --oauth --oauth-scope mcp:tools
haarness mcp auth protected       # standards-based browser OAuth + PKCE
haarness mcp call files search --arguments '{"query":"needle"}'
haarness mcp resources files
haarness mcp read files file:///workspace/README.md
haarness mcp prompts files
haarness mcp prompt files review --arguments '{"target":"src"}'
haarness mcp disconnect files
haarness mcp reconnect files
haarness mcp logout protected
haarness mcp get remote # hides env/header values and URL/argument credentials; add --show-secrets explicitly
# connection and sign-in errors quote a server's address without its user name, password or keys
haarness mcp disable remote
haarness mcp tools files
haarness workflow list
haarness workflow run audit --args '{"scope":"server"}'
haarness schedule add --name morning-review --cron '0 9 * * 1-5' --model openai/gpt-5.6 --prompt 'Review this workspace and summarize urgent work'
haarness schedule add --natural 'in 30 minutes to check the release status' --model openai/gpt-5.6
haarness schedule list
haarness schedule history morning-review
haarness schedule run morning-review
```

## Servers and clients

A plain `haarness`, `haarness tui`, `haarness run`, or workflow command starts
or reuses one loopback workspace server. The TUI, the CLI and Desktop are all
clients of that server: switching chats, `/new`, closing the terminal, or losing an SSE connection
only detaches that client and does not cancel turns, child agents, or workflows.
Reopening Haarness hydrates an authoritative process-wide activity snapshot and
continues streaming. Use `/cancel` for one turn or `haarness serve --stop` for
an explicit server-wide shutdown. `haarness run` is the exception: Ctrl-C (or
SIGTERM from a cancelled CI job) stops its chat's turns, helpers and workflows
on the server before it exits with status 130 (143), and so does closing the
terminal window a run is attended in (SIGHUP, status 129; the second
hang-up a closing shell such as zsh sends does not cut that stop short, while
a second Ctrl-C or SIGTERM still exits at once); a run without a terminal,
such as one under `nohup`, leaves SIGHUP as it found it. A permission prompt during
`haarness run` is asked at the terminal; without one (a script, CI), the run
refuses it and stops, naming what was asked: allow it with a permission rule,
or pass `--yolo`. After an ungraceful server/process failure,
Haarness preserves recorded transcript work and marks orphaned effects
interrupted; it never blindly replays shell, edit, or other side-effecting tools.
Any active goal is restored as paused—not falsely pursuing—until new user input
or an explicit resume safely restarts it.

`haarness acp` exposes the selected workspace as an ACP v1 agent over
newline-delimited JSON-RPC on stdin/stdout. It starts or reuses the same
workspace server as the TUI and CLI, so editor-created sessions are durable and
visible everywhere. Configure an ACP client to launch `haarness acp`, optionally
with `--workspace`, `--provider`, `--model`, and `--agent`. Session working
directories must resolve to that workspace; additional workspace roots are
rejected rather than advertised. PDF/image/audio/video prompts and embedded
context, MCP servers, approvals, streamed tool results, and session reload are
supported. Approvals and tool calls of the subagents and workflow agents a
prompt starts reach the client on the session itself, while that prompt runs:
their titles start with the subagent's name (`Write the proof · Edit notes.md`,
from the `task` call's `description`, otherwise its chat title) and `_meta`
carries `{"haarness": {"childSessionId", "childTitle"}}`. A subagent's own
messages and plan stay in its chat. The client is asked one request at a
time; the others wait. A request the server settles first (answered in
another Haarness app, or covered by a sibling's Reject, which rejects every
request in the conversation, or Always) is never shown, or is withdrawn with
`$/cancel_request` when it already is, and an answer that arrives too late is
ignored. A background subagent that asks while no prompt runs is asked when
the next prompt starts, or can be answered from another Haarness app; after a
reconnect or a gap in the event stream, the requests the server holds are read
again, so one raised or settled meanwhile is not missed. "Allow
while Haarness is running" is offered only where the request allows more
than once.

`haarness attach <url>` is the first-class interactive client for an existing
server; `tui --connect` remains an alias-shaped compatibility entrypoint, and
`run --attach` provides the same server-owned chat path without a terminal UI.
All three accept `--session`/`--thread` and `--continue`; interactive and
headless attach also support `--fork`, which clones the chosen session before
new work. An explicit attach validates the server identity, API schema, and
required history capability before opening. Local PDF/image/audio/video media
selected by an attach client is bounded to 20 MiB and uploaded inline, so the server never attempts
to read a path that exists only on the client machine.

Servers bind loopback by default. An explicit `haarness serve` on loopback accepts
connections without credentials unless `--password` / `HAARNESS_SERVER_PASSWORD` is set.
On a network address (`--host 0.0.0.0`) it refuses to start without a password or
token unless `--anonymous` says that unauthenticated access is intended.
Password authentication uses HTTP Basic; the username defaults to `haarness`
and can be set with `--username` / `HAARNESS_SERVER_USERNAME`. Attach, TUI and
headless clients accept the same options. `--token` / `HAARNESS_SERVER_TOKEN`
remain supported for existing integrations. Stop a running server before
changing its authentication settings; `serve --daemon` never silently changes them.

`haarness serve` listens on port 4096, or on a free port (it says so) while another
program holds 4096; a `--port` you give must be free. A folder has one server, which
owns its chats, terminals and schedules. When the terminal UI, Desktop or another
`serve` already started it, `haarness serve` listens at its own address with its own
credentials and passes every request to that server, which keeps its runtime record,
so clients go on finding it. Stopping that `serve` (Ctrl-C, or `POST /v1/shutdown` at
its address) closes only its address; `haarness serve --stop` stops the folder's
server. `serve --yolo` refuses while a server runs: it cannot change how that server
approves.

Desktop's **Connect to server** form requires only a Server URL. Name is optional,
and **Username and password** expands to optional username/password fields. Saved
credentials are encrypted by the operating system. Private local servers started
automatically by Desktop or TUI still use managed internal credentials, which
users do not need to copy. Existing saved bearer connections remain compatible.

Cross-host browser Origins are rejected in every authentication mode. The built-in
listener is HTTP; Desktop requires HTTPS or a local SSH tunnel for remote hosts.
A server controls the selected project's files and tools, so configure a password
when making it reachable by other users. Each project has a separate session
database, runtime record and server lease; project routing keeps the host's
credentials at the gateway and uses private credentials for child daemons.

## Worktrees

`haarness worktree list` groups local checkouts and linked worktrees by
project. `worktree create NAME [--from REF] [--branch NAME] [--path PATH]`
creates a new non-tracking branch and Git worktree; omitted branch/path values
are collision-resistant. `worktree open NAME-OR-ID` starts or attaches that
worktree's independent server. The TUI exposes the same flow through
`/worktree`: its create modal discovers local branches, remote branches, and
tags, validates custom refs before creation, and supports open, reset, and
remove without stopping work in another workspace. Explicit remote attaches
can create and maintain server-side workspaces but cannot reinterpret a remote
filesystem path locally, so the unavailable Open action is hidden and no
server-local path is exposed as a relaunch instruction.
Reset and removal apply only to Haarness-managed Git worktrees, always require
`--confirm`, refuse active servers, and preserve dirty/untracked work unless
the matching discard flags are supplied. Removal from inside the target
directory is refused; branch deletion uses Git's safe merged-branch check.

## Terminal UI

Inside the TUI, type `/` to browse commands. Press **F2** to switch models without submitting your draft (Ctrl+M is Enter in conventional terminals). The catalog includes `/new`, `/sessions` (`/resume`, `/continue`, and `/threads` are aliases), `/worktree`, `/connect`, `/chrome`, `/models`, `/providers`, `/agents`, `/marketplace`, `/plugin`, `/workflow`, `/btw`, `/goal`, `/plan`, `/review`, `/build`, `/explore`, `/compact`, `/diff`, `/terminal`, `/undo`, `/redo`, `/timeline`, `/messages`, `/fork`, `/rename`, `/archive`, `/reasoning`, `/ultracode`, `/mcp`, `/skills`, `/tools`, `/status`, `/tasks`, `/background`, `/cancel`, `/help`, and `/exit`. Use the arrow keys and Enter to select, or Tab to complete a command. Connected providers and their discovered models sort first; selecting a disconnected provider explains how to connect it.

`/terminal` opens a full-screen VT terminal backed by a server-owned workspace PTY. `Ctrl+]` (or `F12`, for keyboards where `]` is hard to reach) detaches without stopping it, `Ctrl+Shift+T` opens another tab (in terminals with enhanced key reporting; elsewhere use `/terminal new`), `Ctrl+Shift+W` closes and terminates the active tab, and `Alt+Left/Right` switches tabs. A shell that exits closes its tab, and closing the last one returns to the chat. Reopening `/terminal` reconnects and replays bounded output.

The server keeps the last two megabytes of a terminal's output. Reopening `/terminal` after more output than that replays from the first whole line still kept, in the terminal modes the dropped output left on (alternate screen, cursor and keypad keys, mouse reporting, bracketed paste).

A program that turns on application cursor keys (vim, less, htop and other full-screen programs) receives the arrows, Home and End as `ESC O x`, which is what the `xterm-256color` entry its `TERM` names says they send.

Bare `/tools` opens the searchable runtime catalog with enabled, resident, deferred, source/group, and estimated-token state; Enter toggles durable workspace availability. `/tools lazy auto|always|off` controls context scaling and `/tools lazy reset` returns to the configured/default mode, while `/tools show|hide` retains the transcript-detail toggle.

`/diff` opens a full-screen review: a nested file tree, syntax-highlighted patches with real line numbers and word-level emphasis, and a line cursor. `/diff` starts on the Git working tree, `/diff branch` compares the current branch with its detected base, and `/diff last` shows the latest completed mutating turn — and `s` cycles between the three without leaving the viewer. Inside it, `tab` moves between the tree and the patches, `n`/`p` jump between edits, `]`/`[` between files, `v` switches side-by-side and stacked layouts, `x` cycles context lines (4, 12, 50, all), `t` hides the tree, `o` shows one patch at a time, `r` marks a file reviewed, `c` queues a comment on the line under the cursor, and `?` lists every shortcut. Queued comments are handed to the composer when the viewer closes, so a review travels with your next message. Layout choices persist between sessions.

`/undo` and `/redo` walk paired, conflict-checked workspace checkpoints one change at a time and report how far the history reaches in either direction; undo safely stops an active turn first, and submitting new work after undo confirms that boundary. `/timeline` (`/history`) lists every checkpoint in the session by the request that produced it and undoes back to the one you pick, one conflict-checked step at a time — a human edit in the way stops the walk and says so rather than clobbering it.

Every discovered workflow is also a first-class slash command: a definition whose invocation is `release` runs as `/release [plain text or JSON]`; plugin workflows keep their namespaced invocation. `/workflow` opens a searchable dashboard of reusable definitions and durable runs and accepts run/inspect/pause/resume/cancel/delete commands.

Drop a local PDF, image, audio file or short video into the terminal to attach it. Attachments are capped at 20 MiB total for inline provider requests, and Backspace removes the latest chip.

`/usage` (**Alt+U**) lists the session's tokens, cost and context, and, once a request has finished cleanly, `Last request: first token 1.2s · 85 tok/s`: the wait from sending the newest timed request to its first streamed content, and its output tokens per second while they streamed (left out under half a second of streaming). The sidebar's usage section shows the same figure under the token counts. The server records both times on each request's usage item (`first_token_ms`, `stream_ms`), read off a monotonic clock, with the tokens that streamed (`stream_tokens`, counted with the model family's token weights); stopped and failed requests record none of them. A tool call counts from its first argument fragment. The speed divides the streamed tokens, not the provider's output count, which also holds hidden reasoning produced before the first token.

Use **Alt+M** or **`/messages [search]`** to browse prompts and replies, jump to an earlier message, read/copy its full text, add it to a draft, or fork at that message. See [TUI message browser](TUI-MESSAGES.md).

See [Side questions with `/btw`](SIDE-QUESTIONS.md) for isolated questions that leave the main transcript unchanged.

`/ultracode` (or `/reasoning ultracode`) is a durable session preference for difficult work and independent verification, not a command to orchestrate every request. A standalone `ultracode` in an ordinary prompt enables the same reasoning policy for that turn only. Each enabled turn receives conservative execution-shape guidance, child tasks inherit the mode, and the runtime selects the strongest concrete reasoning effort advertised by the active model (Claude-compatible catalogs cap at `xhigh`). Models that do not publish concrete efforts keep their provider default instead of receiving a guessed value. `haarness run --ultracode` applies the same one-turn semantics from the CLI. Focused and short sequential work stays in the main agent; a few isolated investigations use task subagents; only broad, genuinely multi-stage work uses one durable top-level workflow. Workflow completion is event-driven, repeated model polling is refused, and workflow children cannot recursively launch workflows unless their definition explicitly opts in.

## Agents, goals and workflows

The `task` tool creates a durable child session. It inherits the parent model,
reasoning mode, automatic-approval setting, and permission ceilings unless a
configured child agent pins a model or narrows tools. Calls are foreground by
default. Passing `background: true` returns a child ID immediately; pass that
ID back as `task_id` to continue the same child context. If it is still
running, the follow-up is queued without another child or completion watcher. The same
child continues on the server and its terminal result is injected into the
parent automatically. `/background [child-id]` promotes an already-running
foreground child without cancelling or restarting it, and `/tasks` opens its
live transcript. `haarness run --wait-background` drains background subagents
and workflows for headless callers; without it they remain owned by the
workspace server.

Every provider request receives one provider-neutral system scaffold with the
exact provider/model ID, working directory, git-repository status, OS,
architecture, and the thread's stable creation date. The global `AGENTS.md` in
the Haarness configuration folder is layered before workspace `AGENTS.md` files;
each source is labeled. Global instruction imports are
trusted configuration, while project imports are confined to the workspace.
Custom-agent role text augments this runtime contract rather than replacing
the environment or safety facts.

`/goal <objective>` persists the objective and starts pursuing it immediately (input beyond 4,000 characters is retained through the limit and marked `[truncated]`). While the goal is active, the runtime re-presents an untrusted user-role goal doctrine on every logical continuation, accounts non-cached input plus output tokens and elapsed wall time, and allows the model to exit only through `update_goal` after proving completion or a recurring blocker. New goals negotiate 2–5 proposed completion criteria through the normal inline multi-select/custom-answer UI; completion evidence is checked against every approved criterion. Bare `/goal` opens management; exact `status`, `edit`, `pause`, `resume`, `clear`, `complete`, and `block` controls manage state, while longer text remains a literal objective. Clear is confirmed and completion opens localized Edit/New/Clear/Dismiss actions. `haarness run --goal` exposes the same lifecycle headlessly, accepts an ordinary multi-word objective, can target or fork `--session`/`--continue`, resolves criteria noninteractively, and returns success only for an achieved goal.

See [WORKFLOWS.md](WORKFLOWS.md) for workflow roots, source formats, context APIs, lifecycle behavior, and the native-code trust boundary.

## Schedules

Schedules are stored as owner-private JSON under the Haarness configuration
root, while run records, locks, and logs live under its state root. Enabled jobs
are registered with the current user's native scheduler: launchd agents on
macOS, `systemd --user` service/timer pairs on Linux, and per-user Task
Scheduler tasks on Windows. Interval schedules have a one-minute minimum;
five-field cron and one-shot RFC 3339 timestamps are also accepted. Windows
cron conversion intentionally supports the realistic daily/weekly subset and
rejects expressions it cannot represent faithfully. On Linux an interval
timer first fires one interval after it starts, then one interval after each
run. One-shot jobs disable themselves after their terminal run, and an overlap
is recorded as skipped. A run keeps its overlap lock for as long as it runs;
the next run clears a lock whose process has exited, or whose process ID now
belongs to a process started after the lock was taken. Where the process
cannot be checked (Windows, or a lock without a readable process ID), a lock
older than an hour is cleared.

Each run creates an ordinary durable Haarness thread using the selected
provider/model and the schedule's captured workspace. Headless runs explicitly
approve tool permissions and hide the interactive question tool, including in
delegated workflow agents, so they cannot wait forever for a detached terminal.
This grants application-level tool permission only; it does not bypass OS
permissions. Provider credentials are read from the normal stored `haarness
auth` records or environment at run time—no keytab or copied secret is placed
in a scheduler definition. Workspace servers and their session stores are
namespaced, so scheduled work in one registered workspace does not collide
with an active server in another workspace under the same data root.

## Plugins, skills and tools

Haarness loads only its own installed and explicitly configured plugin bundles. It never imports Claude Code settings, plugins, hooks or MCP configuration; Claude Code instructions (`CLAUDE.md`), agents, commands and skills load only when you turn them on under `compatibility.claude_code` (see [Configuration](#configuration)). Commands, skills, nested agents, hooks, workflows, manifest or `.mcp.json` MCP servers, and dynamic MCP header helpers are mounted together. Commands, invocable skills, and agents use collision-safe plugin namespaces; plugin processes receive `CLAUDE_PLUGIN_ROOT` plus `HAARNESS_PLUGIN_ROOT` and compatibility aliases. `/plugin` lists them; `/plugin install <name@marketplace|local-path>`, `/plugin update|enable|disable|remove`, and `/plugin marketplace add` manage Haarness-owned installs. Configuration, provider definitions, skills, commands, agents, hooks, workflows, MCP definitions, and plugin roots hot reload as one generation; changes wait for active turns to become idle, and an invalid edit leaves the last-known-good generation running. The CLI accepts both `haarness plugin marketplace add <source>` and the shorter `haarness plugin marketplace <source>`. The agent can do the same when asked: its `marketplace_manage` tool lists registered plugin marketplaces (`plugin_marketplaces`), registers one (`add_plugin_marketplace`), and installs, updates, enables, disables or removes `name@marketplace` plugins. A source given as a URL with a token in it (`https://oauth2:<token>@git.example.com/team/market.git`) is still cloned as written, but a failed clone names it, and every address in git's own error, with the user name, password and credential-named query parameters as `<redacted>`.

Explicit administration requests expose `skill_manage`, `plugin_manage`, `mcp_manage`, `agent_manage`, and `command_manage`. Durable project knowledge uses the `memory_list`, `memory_read`, `memory_search`, `memory_save`, and `memory_delete` tools; it is stored under Haarness data rather than committed into the workspace, and its bounded index is refreshed into subsequent turns. Lifecycle requests expose `task_abort`, `session_manage`, `session_rename`, `session_message`, `session_wait`, and `agent_switch`. These schemas are intent-gated (or fully available in Ultracode) so ordinary coding turns do not pay for a large rarely-used tool catalog. Their schemas intentionally omit marketplace search, installation, publishing, updates, and versions. Managed skill, plugin, MCP-definition, command, agent, and memory mutations queue the same idle-time generation refresh; operational MCP reconnect, disconnect, authentication, and logout actions remain live. Agent switching requires user approval and continues in a fresh turn.

Tool availability is enforced twice: disabled schemas are omitted from provider requests, and a fabricated/direct call is rejected before execution. Lazy mode applies after permissions and intent gating. Essential coding tools and configured pins remain resident; optional native/MCP schemas are replaced by the compact `load_tool` search schema when the estimated savings crosses the `auto` threshold (or unconditionally in `always` mode). A loaded schema appears on the next provider round and remains promoted for that session, with a 64-tool bound. Catalog estimates use deterministic serialized-schema bytes rather than claiming model-specific tokenizer precision.

Marketplace plugin sources accept `.` or `./` when the marketplace root is itself the plugin. Git sources (`github`, `url`, `git-subdir`) honour `ref` (a branch or tag) and `sha` (a full commit id, which wins), so `install` and `update` fetch exactly the pinned commit; a `url` source may also name a `path` inside the repository. Other relative sources must identify a safe in-marketplace path, while `git-subdir` must identify a real child path. Haarness materializes relative plugins into an owned cache, preserves internal symlinks, excludes `.git`, rejects symlinks that escape the plugin root, and never removes the registered marketplace when uninstalling a root-source plugin. Marketplace and plugin names are one folder name each: a name with `/`, `\`, `:` (on Windows `C:` names a drive), a control character, or `.`/`..` is refused.

An MCP server's `env` and `headers` values may name a secret kept in the system's keychain instead of holding it: `"Authorization": "Bearer ${keychain:github-token}"`. Store it with `haarness mcp secret set github-token` (a hidden prompt, or standard input when piped; `haarness mcp secret remove` takes it out again), or in Desktop with the key toggle a secret-looking value offers in **Add MCP server**. Entries live under the service `haarness-mcp` with the entry's name as its account, reached through the system's own interfaces so a secret never passes through a command line or a script: the login Keychain on macOS, the Credential Manager on Windows (whose entries hold at most 2560 bytes), and the Secret Service of GNOME Keyring or KWallet on Linux, through `secret-tool` (from `libsecret-tools`) with the secret on standard input. A name starts with a letter or digit, then uses letters, digits, `.`, `_` and `-`. Haarness reads an entry only when the server starts (each lookup waits at most 10 seconds, or the server's connection timeout when shorter), once per entry, and never logs it; saving or editing settings keeps the reference as written, and comparing settings for a reload never reads the store, so an updated secret is used on the server's next start. An entry that is missing fails that server with “`${keychain:github-token}` is not in the Keychain; store it with `haarness mcp secret set github-token`”, and one the store refuses or cannot give says so, naming the reference and never a value; `${keychain:NAME:-fallback}` uses the fallback when the entry is missing or the store cannot be reached. A profile carries the reference and never the secret.

Who can read an entry depends on the store. On macOS an entry Haarness stores belongs to Haarness: another program, `security` included, has to ask you first, and a release signed with the same Developer ID keeps reading it after an update without asking (a development build, signed anew on each build, asks again); an entry you made yourself with `security add-generic-password -s haarness-mcp -a NAME -w` belongs to `security`, so the first read asks you to allow Haarness (choose **Always Allow**). On Windows any program running as you can read your Credential Manager entries, and on Linux any program in your session can read an unlocked keyring, as with the other secrets kept there. Name keychain entries only in settings you trust: Haarness sends a referenced secret wherever the settings say, to the URL in a server's entry or to the program it starts, so a project's settings that name `${keychain:github-token}` hand that token to the server they configure; project settings load only for a project you opened with its configuration on (see [Configuration](#configuration)).

MCP settings never show their credentials, wherever they are displayed (`haarness mcp get`, the TUI and Desktop settings, the model's `mcp_manage` tool): environment and header values, a password or user name in the server URL, credential-named query parameters such as `?api_key=`, and command arguments such as `--api-key VALUE`, `--token=VALUE` or `-H 'Authorization: …'` read `<redacted>`, while the host, path and other arguments stay readable. A setting saved back unchanged keeps the values it stands for; in edited arguments a hidden value stays with its own flag (`--token <redacted>` keeps the token wherever it moves), and one that cannot be matched without doubt is refused with a request to give it in full. A server that cannot be reached is named the same way in its status.

An MCP tool's text, images and audio reach the model as its result. A tool that answers with structured content and no text (as one with an output schema may, or beside only an image, audio or a link) has that content passed on as JSON text, to the model and to a workflow's `tool()` call alike; a failure with no text at all says which tool failed. Structured content that uses a name Haarness fills itself in a result's metadata (`attachments`, `diffs`, `mcp_app`) is kept under `structuredContent` instead, so a tool cannot add media, file changes or a page by naming them. A tool that an [MCP App](https://modelcontextprotocol.io/extensions/apps) marks as one only its own page calls (`_meta.ui.visibility: ["app"]`) is left out of the model's tools, `haarness mcp tools` and workflows; a visibility list naming neither `model` nor `app` counts as both.

When a connected server declares resources (documents, records and other data it exposes beside its tools), the model gets two more tools: `mcp_resource_list` lists the resources and resource templates of one server, or of every server that has resources, each entry naming the server to read it from, and `mcp_resource_read` reads one by server and URI. They are offered only while such a server is connected (or reconnecting with the same settings), so a session whose servers expose tools only keeps the tool list it had, and with it its prompt cache; with lazy tools on they load like other MCP schemas. A read returns text as text, cut with a note that says where the whole of it was saved when it is long (as other tools' output is), images and PDFs as attachments for models that take them (at most 10 MiB per read), and other binary content by its type and size, never as base64 text. A read or listing of a server that is not connected, or declares no resources, names the servers that do. A server whose tools a rule or setting takes away entirely (`"mcp__docs__*": "deny"`, or `"mcp__docs__*": false` under `tools`, for the chat or its agent) keeps its resources from the model too: a listing of every server leaves it out without asking about it, a read naming it is refused, and with no resource server left the two tools are not offered. A rule about one of its tools leaves its resources alone. A listing or read waits as long as the server's connection timeout (`timeout_ms`, 30 s by default) and then fails saying so; Stop ends it at once. Either way the server is told a read was cancelled, so its next request is not held up behind it. Both tools run without asking unless a permission rule says otherwise; their permissions are `mcp_resource_list` (the server name) and `mcp_resource_read` (`<server>:<uri>`), described under [MCP resources](PERMISSIONS-AND-EXECUTION.md#mcp-resources). Desktop rows read “Read an MCP resource `docs://setup`” and “Listed MCP resources `docs`”; the TUI names the tool with the resource or server beside it.

A local (stdio) server answers one JSON message per line, and one message may hold up to 64 MiB, so results of 20 MB and more pass. A server whose message passes that (one stuck writing without ever ending its line) is stopped with its whole process tree before it can fill memory: its status reads "Its reply was larger than 64 MB, so Haarness stopped it. Reconnect it to use it again.", whether that happened while it started or later, and a tool call, resource read or prompt request waiting on it ends with `MCP server "<name>" sent a reply larger than 64 MB, so Haarness stopped it. Calling it again fails until the user reconnects it.`, so the model tells you instead of trying again. Haarness does not restart such a server on its own, for a later call (which gets the same answer) or on a configuration reload that leaves its settings as they were; **Reconnect** in Settings → MCP or `haarness mcp reconnect <name>` starts it again, as does an edit of its own settings. The log names the server and the limit, never what it sent.

Haarness tells every MCP server it connects to that it renders [MCP App](https://modelcontextprotocol.io/extensions/apps) pages (the `io.modelcontextprotocol/ui` extension), unless that server's interactive apps are turned off with `"apps": false` in its entry (Desktop's **Interactive apps** switch writes it; on is the default and is saved as no key). A server with its apps off is a plain MCP server to Haarness: it is not told pages render, its results carry no page, its `ui://` pages are not served, and changing the setting reconnects it once the configuration reloads. Otherwise a server may pair a tool with a `ui://` page (`_meta.ui.resourceUri`, or the older `_meta["ui/resourceUri"]`). The model, workflows and the TUI still get the tool's text; a successful result of such a tool also carries `mcp_app` in its metadata, beside its structured content: the server, the page's URI, the tool as declared and the whole `CallToolResult` for the page (images, audio and binary resources are left out of that copy past 512 KB, and a result still larger is left out with `result_too_large: true`). Haarness Desktop shows the page under the tool's row (see [Desktop](DESKTOP.md#mcp-app-pages)); the TUI says under the row that the tool has an app to open in Desktop. A page cannot yet call tools, change what the model reads, download files or go full screen.

The commercial extension boundary is provider-neutral. Remote MCP authorization uses only public OAuth metadata discovery, authorization code + PKCE, optional dynamic client registration, and owner-only local credential storage. Haarness contains no company-internal identity broker, authentication transport, hostname, marketplace, or service dependency.

## Providers

Connecting a provider is one flow everywhere. In the TUI, `/connect` (alias `/login`) opens a picker of every provider with how it can be connected; choosing one either asks for a key (masked, with the console link and the environment variable that also works) or signs in: ChatGPT, xAI, GitHub Copilot and Haarness Cloud open the browser, poll the daemon and land in the model picker when the browser comes back. xAI and GitHub Copilot also show a code to enter, and every sign-in offers a code method for SSH sessions and remote servers. `/connect openai` skips the picker; `/providers` remains the manager for disconnecting, custom endpoints and the Router. With nothing connected the picker opens on launch and the home screen says so; `HAARNESS_NO_CONNECT_PROMPT=1` suppresses the launch prompt. Desktop opens the same dialog on first run (see the [Desktop first run](DESKTOP.md#first-run)), from the composer's "Connect a provider" pill, and from Settings → Providers; it uses the management API, so it also works against a remote server, where the device-code method replaces the browser callback. The sign-in issuer can be pointed at a fixture with `HAARNESS_OPENAI_OAUTH_ISSUER` (and `HAARNESS_OPENAI_OAUTH_CALLBACK_PORT`), which is how `scripts/test-tui-connect.sh` and the Desktop e2e suite exercise the OAuth handshake without OpenAI. Any web page can send a request to a sign-in's local callback port (ChatGPT's is fixed at 1455), so the ChatGPT, Haarness Cloud and MCP sign-ins end only on a return that carries the `state` of the authorization page Haarness opened, with a code or the provider's error; any other request gets `400` or `404`, never ends the sign-in or words its error, and the sign-in keeps waiting until its usual timeout. Each connection is read on its own, so idle ones (a page's preconnect, a browser's spare socket) do not hold up the real return, and one that fails before it is accepted (its client reset it, or no file descriptor was free) ends only itself, not the sign-in. A sign-in whose port could take no connection at all says why when it times out. A sign-in link a server sends (an MCP server's authorization page; the verification page of an xAI, GitHub Copilot or Haarness Cloud device code) is shown and opened only when it is http or https: a `file:` link or an app's own scheme would open a local file or start an app, so it stops the sign-in with `MCP server "name" sent a file: sign-in link; only http and https links open` (or the device-code equivalent). What is printed and opened is the parsed address, and a device code with control characters stops the sign-in too, so nothing a server sends can write escape sequences to the terminal.

Provider environment variables override stored credentials: `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`. Native Gemini checks `GOOGLE_API_KEY`, `GEMINI_API_KEY`, then `GOOGLE_GENERATIVE_AI_API_KEY`, in that order, before falling back to a stored `gemini` credential. `haarness auth login openai` implements a public-client PKCE browser flow, while `--method headless` uses its ChatGPT device-code flow; neither installs Codex nor requires registering a separate OpenAI app. `haarness auth login xai` implements the RFC 8628 SuperGrok device flow, and `haarness auth login github-copilot` implements GitHub's device flow with its public-client authentication and API headers. Access, refresh, expiry, and ChatGPT account records use typed `auth.json` fields. Haarness refreshes ChatGPT five minutes and xAI two minutes before expiry, coalesces concurrent refreshes, persists rotated tokens (a request stopped mid-refresh returns at once while the refresh still saves, so the next one never sends a spent refresh token; a refresh gives up after 30 seconds without an answer, and an exiting process waits up to 5 seconds for one still running), scopes ChatGPT requests with its account/residency headers, and uses an inter-process compare-before-write guard so a late refresh cannot undo logout or re-login. Anthropic/Claude and Gemini use API keys because built-in subscription OAuth is unavailable for those providers; `haarness auth login anthropic` and `haarness auth login gemini` therefore prompt for a key. A running server reads native and compatible-provider credentials live, so login/logout and refreshed tokens do not require restart. What each credential can run is discovered rather than assumed: an OpenAI API key lists the public `/models` catalog, and a ChatGPT sign-in reads that account's own Codex catalog, which carries the models the subscription serves, the reasoning tiers each accepts, hidden internal models, and retirement notices. Pickers offer only the listed, non-retiring models; a model outside the account catalog fails before the request with the available alternatives, and provider error bodies (including the ChatGPT backend's `detail` field) are surfaced instead of a bare HTTP status.

The `read` tool returns UTF-8 text normally and exposes PDF, image, audio, and
video files as provider-visible attachments instead of decoding binary bytes as
text. Direct prompt attachments use a 20 MiB aggregate limit; Gemini retains
its 10 MiB aggregate adapter budget for inline request overhead. OpenAI,
Anthropic, and Bedrock accept PDF/images; Gemini accepts PDF/image/audio/video.
OpenAI-compatible Chat endpoints receive their native file, image, or MP3/WAV
shape where available, but the selected gateway and model remain authoritative.

`webfetch` reads one HTTP or HTTPS address and turns an HTML page into Markdown,
leaving out its scripts, styles and embedded frames. Reading stops as soon as a response
passes 6 MiB, also when the server sends no length or a wrong one; a fetch that
has not finished after 60 seconds ends with an error, and Stop ends it at once.

`image_generate` and `video_generate` are lazy, side-effecting tools that always
require the `media_generate` permission unless an explicit later rule allows
them. Image generation uses `HAARNESS_IMAGE_GENERATE_URL`, or the OpenAI Images
endpoint when `OPENAI_API_KEY` is present. Video generation uses
`HAARNESS_VIDEO_GENERATE_URL`. Custom endpoints must be HTTPS (exact loopback
HTTP is accepted), receive only `HAARNESS_MEDIA_API_KEY` when configured (the
OpenAI key is reserved for Haarness's implicit OpenAI Images endpoint), and must
return JSON with a `b64_json`, `base64`, or `data` field plus an optional `mime`, `mime_type`, or
`mimeType`. The response is read up to 72 MiB and refused beyond that, whether
or not it declares its length. Generated bytes are MIME-signature checked, capped at 50 MiB, and
written collision-safely under ignored `.haarness-chat-attachments/` files.

The cloud transports are first-class rather than compatibility labels. Azure uses `AZURE_OPENAI_BASE_URL` or `AZURE_RESOURCE_NAME`, then `AZURE_OPENAI_API_KEY`/`AZURE_API_KEY`, a stored `azure` credential, or `az account get-access-token`. Vertex uses `GOOGLE_VERTEX_PROJECT` (with the usual Google project aliases), `GOOGLE_VERTEX_LOCATION` (default `us-central1`), an explicit Google access token, a stored `google-vertex` credential, or the `gcloud` application-default/user access-token commands. Copilot uses its device login or `GITHUB_COPILOT_TOKEN`/`GH_TOKEN`/`GITHUB_TOKEN`, and filters authenticated `/models` discovery to Chat Completions-capable entries. Bedrock uses `AWS_BEARER_TOKEN_BEDROCK`, a stored `amazon-bedrock` bearer credential, environment access keys, or the selected `AWS_PROFILE` in the shared credentials file; requests use Bedrock Converse with SigV4 service signing and `AWS_REGION`/`AWS_DEFAULT_REGION` (default `us-east-1`). Web-identity, ECS/EC2 metadata, SSO, and automatic role assumption remain the responsibility of an external credential helper that exports credentials; Haarness does not contact metadata endpoints implicitly. Bedrock Converse currently emits its provider-neutral events after the complete response rather than incrementally decoding AWS event-stream frames.

The `OPENAI_COMPATIBLE_*` family configures the legacy compatible endpoint. Custom providers can use the OpenAI Chat Completions, Anthropic Messages, or Gemini GenerateContent wire protocol, with the same tool, structured-output, reasoning, media, cancellation, usage, and error transforms as the corresponding native provider. First-class OpenAI Chat Completions presets also recognize `GROQ_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`, `DEEPSEEK_API_KEY`, `TOGETHER_API_KEY`, `FIREWORKS_API_KEY`, `OPENROUTER_API_KEY`, `CEREBRAS_API_KEY`, `SAMBANOVA_API_KEY`, `NVIDIA_API_KEY`, `ZAI_API_KEY`/`ZHIPU_API_KEY`, `DEEPINFRA_TOKEN`/`DEEPINFRA_API_KEY`, `HF_TOKEN`, and `PERPLEXITY_API_KEY`. Built-in providers and a small documented fallback catalog remain visible before connection; after a key is configured, Haarness replaces fallbacks with authenticated model discovery when that provider supports it. `haarness auth set` writes owner-only typed API records to `auth.json`; legacy string records remain readable.

`haarness provider add` and `haarness provider remove` manage custom definitions without putting API keys in the config. For example:

```bash
haarness provider add local-vllm --url http://127.0.0.1:8000/v1 --no-auth --model my-org/my-model
haarness provider add company-claude --url https://models.example.com/anthropic/v1 --protocol anthropic-messages --api-key-env COMPANY_MODELS_KEY --model company-sonnet
haarness provider add company-gemini --url https://models.example.com/gemini/v1beta --protocol gemini-generate-content --api-key-env COMPANY_MODELS_KEY --model company-flash
haarness provider add company-azure --url https://example.openai.azure.com/openai/v1 --protocol azure-openai --api-key-env AZURE_OPENAI_API_KEY --model deployment-name
haarness provider add company-vertex --url https://us-central1-aiplatform.googleapis.com/v1/projects/example/locations/us-central1/publishers/google --protocol google-vertex --api-key-env GOOGLE_OAUTH_ACCESS_TOKEN --model gemini-2.5-pro
haarness auth set company-claude
```

Use `--project` to write the definition into the project's configuration instead. `--header NAME=VALUE` adds a validated request header; quote `--header 'Authorization=${COMPANY_MODELS_TOKEN}'` to resolve its value from the environment without storing the secret. Cleartext custom-provider URLs are accepted only for exact loopback hosts. Adding, removing, or changing a provider definition hot reloads a running workspace server after active work becomes idle, including a refreshed automatically discovered model catalog. Credential login, logout, and refresh also apply live to configured model IDs. Haarness does not include company-internal identity brokers, endpoints, provider names, or credentials.

The same portable adapter includes Moonshot AI, SiliconFlow (global and China), Nebius Token Factory, Requesty, NovitaAI, Ollama Cloud, Llama API, ModelScope, Upstage, DigitalOcean Gradient AI, Weights & Biases, and Chutes. Their conventional variables are `MOONSHOT_API_KEY`, `SILICONFLOW_API_KEY`, `SILICONFLOW_CN_API_KEY`, `NEBIUS_API_KEY`, `REQUESTY_API_KEY`, `NOVITA_API_KEY`, `OLLAMA_API_KEY`, `LLAMA_API_KEY`, `MODELSCOPE_API_KEY`, `UPSTAGE_API_KEY`, `DIGITALOCEAN_ACCESS_TOKEN`, `WANDB_API_KEY`, and `CHUTES_API_KEY`. Heterogeneous model catalogs use a curated text-and-tools fallback instead of exposing incompatible image, embedding, guard, or non-tool entries.

## Interface language

Interface language defaults to English regardless of the OS locale. Use `/language` in TUI or Desktop, or Desktop Settings → General → Language. See [supported languages and translation behavior](../locales/README.md).

## Configuration

Global configuration lives in `~/.config/haarness/` on every OS, including macOS (`$HAARNESS_HOME/config` when `HAARNESS_HOME` is set, otherwise `$XDG_CONFIG_HOME/haarness` when that is set). Haarness writes `haarness.json` there (`haarness.jsonc` is read too); an existing `config.toml` beside it is still read and accepts the same sections. The terminal UI reads its `[tui]` settings from that `config.toml`; settings it once kept in `~/Library/Application Support/dev.haarness.haarness` on macOS still apply until the shared file sets them. Project configuration lives in `haarness.json` or `.haarness/haarness.json` (either may be `.jsonc`) and, like project integrations, loads automatically in Desktop, TUI, CLI, and SDK-launched servers. See [Configure Haarness](https://haarness.ai/docs/cli/configuration) and [configuration scopes](CONFIGURATION-SCOPES.md). CLI flags override project configuration, which overrides global configuration. To use only global configuration, pass `--disable-project-config` or set `HAARNESS_DISABLE_PROJECT_CONFIG=1`. This opt-out also skips project commands, agents, skills, hooks, MCP servers, extensions, workflows, and instruction files. Individual disabled features and ordinary tool permissions still apply. The old `--trust-project` flag remains accepted for compatibility.

The example below uses the TOML form. In `haarness.json` the same sections are JSON objects, for example `{"defaults": {"provider": "openai", "model": "gpt-5.6"}}`.

```toml
[defaults]
provider = "openai"          # used when a new thread does not name one
model = "gpt-5.6"
agent = "build"
reasoning_effort = "medium"  # none|minimal|low|medium|high|xhigh|max
ultracode = false             # durable default for newly created sessions
output_style = "concise"      # concise|explanatory|formal|minimal, or an output-styles/<name>.md file

[tui]
language = "en"               # Explicit UI language; /language opens the 17-language picker

[tui.statusline]
command = ["my-haarness-status"] # receives JSON on stdin and HAARNESS_STATUS_CONTEXT
refresh_ms = 2000
timeout_ms = 5000

[permissions]
# Decisions are allow|ask|deny; later rules override earlier ones.
# The defaults allow tools, while external directories, secret
# `.env` reads, agent switches and generated media ask. `edit` governs write, edit, and
# apply_patch together; legacy write/apply_patch spellings remain aliases.
edit = "ask"
[permissions.external_directory]
"~/Downloads/*" = "ask"       # ~ and $HOME expand in permission patterns
[permissions.bash]
"git status*" = "allow"
"git push*" = "ask"

# Optional guardrail; omit it for the unbounded default.
# [tools]
# max_rounds = 64

# Individual tool policy. Project values extend/override trusted global values;
# live TUI/API overrides are workspace-scoped and persist in the local DB.
[tool_availability]
# webfetch = false
# mcp__github__delete_repository = false

[lazy_tools]
mode = "auto"               # off|auto|always
min_saved_tokens = 2048     # auto-mode threshold; small contexts lower it
pin = []                     # exact ids or '*' wildcard patterns

[compaction]
enabled = true              # false disables automatic compaction; /compact remains available
threshold = 0.85            # optional early trigger, 0 < threshold <= 1
tail_turns = 2              # recent user turns retained verbatim after the summary
preserve_recent_tokens = 24000 # upper bound; also capped to half the model-safe input
# reserve_output = 32000    # optional extra reserve; model output metadata is always honored

# Built-in language servers are enabled by default and start lazily only for
# matching files. Missing executables fail open and appear as missing in /status.
[lsp]
enabled = true               # false disables every language server
builtins = true
diagnostic_timeout_ms = 2000
request_timeout_ms = 10000

# Disable one built-in without disabling the rest.
[lsp.servers.rust]
enabled = false

# Or add/replace a server. Commands are argv arrays and never use a shell.
[lsp.servers.acme]
command = ["acme-language-server", "--stdio"]
extensions = [".acme"]
environment = { ACME_MODE = "workspace" }
initialization = { diagnostics = true }
[lsp.servers.acme.language_ids]
acme = "acme-language"

# Formatting is opt-in. Enabling it activates installed built-ins; unavailable
# commands are skipped. Formatters run after write/edit/apply_patch and before
# LSP diagnostics, so tool diffs and snapshots describe the final contents.
[formatter]
enabled = true               # false (the default) disables every formatter
builtins = true

[formatter.formatters.prettier]
enabled = false              # disable one built-in

[formatter.formatters.acme]
command = ["acme-format", "--write", "$FILE"]
extensions = [".acme"]
timeout_ms = 30000

[experimental.goal_harness]
token_budget = 40000         # optional; omit for an unbounded goal
max_turns = 20               # autonomous continuations after the initial turn

# Workspace checkpoints behind /undo, /redo, and /diff last.
[snapshots]
enabled = true
retention_days = 7           # 0 keeps every checkpoint forever
keep_per_thread = 20         # always restorable, however old

# Override a built-in preset. No secret is stored in this TOML.
[providers.openrouter]
protocol = "openai-chat-completions"
api_key_env = "OPENROUTER_API_KEY"

# Add any OpenAI Chat Completions endpoint. `kind = "openai-compatible"`
# remains a backward-compatible alias for the protocol field/value.
[providers.local-vllm]
protocol = "openai-chat-completions"
name = "Local vLLM"
base_url = "http://127.0.0.1:8000/v1"
models = ["my-org/my-model"] # explicit list disables GET /models discovery
requires_auth = false        # opt in only for an endpoint that needs no key
reasoning = false
images = false
video = false                # generic Chat Completions video=true is rejected
tools = true
context_window = 131072
max_output_tokens = 16384

# Use the native Anthropic Messages wire against a custom commercial gateway.
[providers.company-claude]
protocol = "anthropic-messages"
name = "Company Claude Gateway"
base_url = "https://models.example.com/anthropic/v1"
api_key_env = "COMPANY_MODELS_KEY"
models = ["company-sonnet"]
headers = { X-Tenant = "engineering" }

# Use Gemini GenerateContent, including native image/video and thought-signature handling.
[providers.company-gemini]
protocol = "gemini-generate-content"
name = "Company Gemini Gateway"
base_url = "https://models.example.com/gemini/v1beta"
api_key_env = "COMPANY_MODELS_KEY"
models = ["company-flash"]
video = true

# Override a first-class cloud transport. The API value is read from the
# environment and receives the provider-specific header/auth treatment.
[providers.azure]
protocol = "azure-openai"
base_url = "https://example.openai.azure.com/openai/v1"
api_key_env = "AZURE_OPENAI_API_KEY"
models = ["deployment-name"]

[providers.google-vertex]
protocol = "google-vertex"
base_url = "https://us-central1-aiplatform.googleapis.com/v1/projects/example/locations/us-central1/publishers/google"
api_key_env = "GOOGLE_OAUTH_ACCESS_TOKEN"
models = ["gemini-2.5-pro"]

# Suppress a preset or local-runtime auto-detection.
[providers.ollama]
enabled = false

# Tune a provider Haarness already has (built in, or a local runtime it
# detects) without redefining it. A response that sends nothing, not even a
# keep-alive, for this many seconds is closed and retried; 0 never gives up.
# Default 300, or 0 for a provider on this machine (localhost, 127.0.0.1, ::1).
[providers.anthropic]
stream_idle_timeout = 600

# Bedrock's Converse answers in one piece, so it has a limit on the whole
# answer instead (default 900 seconds; 0 waits as long as it takes).
[providers.amazon-bedrock]
request_timeout = 1800

[mcp.files]
transport = "stdio"
command = "my-mcp-server"
args = ["--root", "."]
enabled = true

[mcp.files.env]
TOKEN = "${MCP_TOKEN}"

[mcp.remote]
transport = "streamable_http"
url = "http://127.0.0.1:8080/mcp"
enabled = true

[mcp.remote.headers]
Authorization = "Bearer ${MCP_TOKEN}"

[mcp.protected]
transport = "streamable_http"
url = "https://example.test/mcp"
oauth = true                  # omit to auto-detect; false opts out
oauthClientId = "public-id"  # optional; otherwise discovery/DCR is used
oauthScopes = ["mcp:tools"]  # optional; otherwise server metadata selects scopes
enabled = true
```

`${NAME}` and `${NAME:-fallback}` in an MCP entry come from the server's environment.
Environment values and headers (and the headers helper's environment) are resolved once,
when Haarness connects; the headers helper is shell text and keeps its references for its
shell, which reads them from that environment (see
[Hooks](CONFIGURATION-SCOPES.md)); in a JSON file the other fields are resolved when the
configuration loads. A variable's value is used as it is, so a `${OTHER}` inside it
reaches the MCP server as that text, and what a headers helper prints is never resolved.
The references stay in the file: `haarness mcp enable`/`disable`, `mcp_manage` and Desktop
edits write only the keys they change, in the entry's own shape, and `haarness mcp get
--show-secrets` shows the entry as written.

Answering an approval with **Always** (`a`) saves the rule shown on the card, which is a pattern rather than the exact call (approving `rm -rf build` allows every `rm` command), for every chat on that server until it stops. **This folder** (`w`) and **Repository** (`r`) answers are saved in the local database and apply after restarts. See [Permissions and approvals](https://haarness.ai/docs/security/permissions#what-does-always-remember). Global skills live in `~/.config/haarness/skills/<name>/SKILL.md` (where `haarness skill create` writes them); project skills live in `.haarness/skills/<name>/SKILL.md`.

The JSON/JSONC form (`haarness.json`, including legacy filename aliases) accepts the same sections plus the ecosystem keys `instructions` (extra instruction files: globs, `~/` paths, absolute paths and `http(s)://` URLs), `skills` (`paths` and remote `urls` indexes), and `compatibility.claude_code` (opt-in `instructions`, `agents`, `commands`, `skills` discovery of Claude Code files, all off by default). Markdown agents accept `hidden`, `color`, `steps`, `disable`, `temperature`, `top_p` and `variant`; commands accept `subtask: true` as `context: fork`; `@path` references in command templates attach file snapshots, directory listings or a subagent delegation hint. See [docs/CONFIGURATION-SCOPES.md](CONFIGURATION-SCOPES.md).

The built-in LSP catalog covers Rust, TypeScript/JavaScript, Python, Go, C/C++, shell, YAML, and TOML. The built-in formatter catalog covers rustfmt, gofmt, Prettier, Ruff, clang-format, shfmt, and Zig. Haarness discovers these commands on `PATH`; it does not silently install or update third-party executables. `/status` shows configured LSP processes as idle, running, missing, stopped, or error, and formatters as available or missing. Project `lsp` and `formatter` sections replace the global section unless project configuration is disabled, so a project can disable either feature completely without affecting other workspaces.

Custom-provider protocols are explicit: `openai-chat-completions`, `anthropic-messages`, `gemini-generate-content`, `azure-openai`, `google-vertex`, `github-copilot`, and `amazon-bedrock`. The last four retain their provider-specific endpoints and authentication instead of being relabeled as generic compatible APIs. Capability overrides are conservative metadata defaults for APIs whose model list omits them; live model metadata wins except where config explicitly overrides it. Perplexity Sonar uses a documented static model fallback and is marked without custom function tools because its provider-specific search tools are not Haarness function tools.

## Architecture

The workspace uses ports and adapters. `haarness-core` owns the agent state machine and traits but cannot depend on HTTP, SQLite, provider DTOs, MCP SDK types, CLI parsing, or OpenTUI. Outer crates implement those ports. The TUI imports only wire contracts and the OpenTUI wrapper; it never links the core runtime.

See [ARCHITECTURE.md](ARCHITECTURE.md) for invariants, [API.md](API.md) for the public server contract, and [OBSERVABILITY.md](OBSERVABILITY.md) for OTLP tracing configuration and the span/privacy contract.

## Verification

```sh
./scripts/test-all.sh
./scripts/test-real-plugins.sh  # optional networked smoke against Anthropic's public marketplace
```

That gate checks formatting, architecture boundaries, Clippy, the complete workspace suite, a release one-binary build, the embedded ABI, REST/provider/recovery flows, exact RGB output, slash-command interaction, a real tmux-driven TUI session, and an isolated installed-plugin smoke that loads a skill, executes a command through a pinned custom agent, exercises real MCP tools/resources/templates/prompts, and verifies disconnect/reconnect.
The separate networked smoke clones the current public `anthropics/claude-code` marketplace into isolated state, installs its real `commit-commands` and `plugin-dev` plugins, discovers their commands, agents, and skills, executes a real command template through the echo provider, and verifies cache isolation and uninstall safety.

## Desktop app

The Electron desktop app lives in `apps/desktop`, with Haarness's own design and a shared workspace server. It includes streaming conversations, approvals, a file editor, diff review, terminals, a built-in browser, setup for [Haarness for Chrome](CHROME-EXTENSION.md), extensions, plugins, and runtime settings.

```sh
npm run desktop:install
npm run desktop:dev
```

See [Desktop](DESKTOP.md) for its first run, development, packaging and verification, and [chat notifications](DESKTOP-NOTIFICATIONS.md).
