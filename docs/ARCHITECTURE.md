# Architecture

## Dependency rule

`haarness-protocol` contains stable wire types. `haarness-core` contains domain behavior and ports. Store, provider, MCP, language-process, server, scheduler, native-terminal, and CLI crates are adapters. Composition happens only in `haarness-cli`. `haarness-language` owns shell-free formatter subprocesses and the stdio JSON-RPC LSP client; the server adapter attaches it to native file tools, while core remains unaware of either protocol.

The core must never depend on `axum`, `rusqlite`, `reqwest`, `rmcp`, `clap`, `crossterm`, or `haarness-opentui`. The TUI must never depend on `haarness-core`, a provider, or a store. The automated architecture check rejects those edges.

`haarness-insights` is a query-only history adapter shared by CLI analytics and
authenticated server endpoints. It reads registry metadata and per-workspace
SQLite snapshots without composing a runtime. The TUI consumes those endpoints
and writes reports locally; it never opens the server's databases directly.

## Runtime invariants

- SQLite is the durable source of truth; events are published after persistence.
- One turn runs per thread. Independent threads run concurrently; child agents run up to `execution.subagent_concurrency` at each delegation depth (four by default).
- Every command, provider stream, tool, question, permission, and child receives cancellation.
- OpenTelemetry is an optional composition-root adapter: core emits provider-neutral `tracing` spans, the server owns HTTP trace-context extraction, and only the CLI configures and flushes an OTLP exporter. Core and server do not select a collector or sampler.
- A task child has one runner regardless of foreground/background state.
  Promotion only releases the parent's wait; it never restarts the child.
  Background completion is delivered only after the launch tool result is
  durable, so provider history cannot observe a result before its call.
- The user item and running turn are committed together before provider work begins.
- An exact request ID retry returns the existing turn and never starts a second runner.
- Restart converts queued/running turns to interrupted. No side-effectful tool is automatically replayed.
- Provider, database, and protocol representations are separate types at their adapter boundaries.
- File-backed runtime state reloads as a last-known-good generation. Provider
  definitions, effective configuration, commands, agents, hooks, skills, MCP,
  workflows, formatter/LSP definitions, and enabled plugin roots are composed
  before the swap. Turn admission and direct tools share a reload gate; an
  active turn keeps its original provider/tool/hook snapshot, while one pending
  generation applies after all turns become idle. Invalid TOML/JSON or provider
  definitions publish a failure event and leave the current generation intact.
- `haarness-ripgrep` embeds a pinned, checksummed upstream binary for the build
  target. Server shell tools and PTYs prepend its private cache directory to
  their child process PATH; login-shell tools restore that entry after profiles
  run. Extraction is offline, locked, atomic, and verifies cached bytes before
  use. Core and protocol remain independent of executable distribution.
- MCP and built-in tools share the same provider tool schema and permission path.
- Formatter and LSP processes are workspace-scoped, lazy, cancellable, and shut down with the server. Formatting completes inside the original mutating tool call before its final diff and diagnostics are returned, so the runtime's existing before/after snapshot pair includes the formatted bytes without introducing a second mutation epoch. Missing optional executables fail open.
- Interactive PTYs are workspace-scoped, process-local server resources. Their raw output lives in a bounded two-megabyte replay buffer with a monotonic byte cursor and is streamed through authenticated WebSockets; it is not transcript or SQLite state. Client detach and session switches leave PTYs running, while explicit tab deletion and server shutdown terminate them. The TUI parses child escape sequences into an in-memory VT screen before drawing OpenTUI cells, so a nested application cannot write control sequences into the outer terminal.
- Provider-facing configuration, memory, task, session, and agent-switch schemas live in the server adapter, while the CLI composition root implements their path/configuration and runtime-lifecycle port. Rare management schemas are intent-gated outside Ultracode. Commands and the bounded project-memory index participate in the idle-time generation refresh; session tools operate on the authoritative runtime so targeted abort cannot stop a sibling or parent. Local/global administration intentionally excludes marketplace operations.
- Scheduler jobs persist independently of a server process. Native launchd,
  systemd-user, or Task Scheduler entries invoke the same executable through a
  hidden trigger command; that command uses the authenticated loopback API and
  ordinary turn/runtime path rather than creating a second execution engine.
- Scheduled turns propagate their explicit non-interactive approval to child
  agents, workflow agents, and automatic background-completion turns, and
  remove the question tool throughout that lifecycle. Stored credentials
  are resolved only when a run starts and are never embedded into native job
  definitions.
- The universal provider system scaffold contains only public product/runtime
  facts: exact provider/model, workspace, git status, platform, architecture,
  and stable thread date. Labeled global/project instruction layers and custom
  agent role text are appended without replacing that scaffold. No private
  identity, endpoint, credential, or company environment is assumed.
- Each request starts with the previous one byte for byte (system prompt and
  every earlier message), so the provider's prompt cache keeps the
  conversation. Context that changes between turns is not in the system
  prompt: before a turn's first request the runtime stores one hidden
  (`synthetic`) user message after the prompt with whatever applies, each in a
  `<system-reminder>` block: `Today's date is now YYYY-MM-DD.` when the
  computer's local date differs from the date the model last saw (the system
  prompt names the chat's start date, in local time), the project memory index
  when it is not the one the model last saw (the first turn, after a save, or
  after a compaction summarized it away; `The durable project memory index is
  now empty.` when it was cleared), and the turn's `UserPromptSubmit` and
  command hook context. What the model last saw is read back from the stored
  messages it can still see, so later requests replay them unchanged. The
  logic lives in `crates/haarness-core/src/turn_context.rs`.

## Native boundary

All unsafe code is confined to `haarness-opentui`. The wrapper embeds a checksummed library built from the vendored OpenTUI v0.5.9 native source, atomically extracts it to an owner-private content-addressed cache, loads only that absolute path, and resolves a pinned symbol manifest before creating a renderer. Its Rust declarations mirror the pinned v0.5.9 ABI exactly: renderer and buffer handles are `u32`, colors are packed `[u16; 4]` RGBA values, and byte lengths use explicit `u32` widths. The wrapper also forwards OpenTUI's terminal-environment allowlist so native capability selection works when hosted through Rust. The client queries OSC 11 before entering the alternate screen to choose a readable dark/light variant; `auto` background mode uses the theme page color on RGB terminals and OpenTUI's transparent/default-background intent on limited-color terminals. In terminal mode the page, sidebar, transcript, code, and status surfaces are transparent as one coherent base layer, while menus, dialogs, selections, and emphasized diff rows keep opaque theme colors. Headless buffer tests, native ANSI-output tests, and an FFI capability test cover the rendering boundary.

## Extension surfaces

`haarness-extensions` adapts trusted TypeScript/JavaScript modules to the core
tool registry and hook ports through bounded stdio RPC to optional Bun
subprocesses. The CLI owns installation, discovery, and global opt-in. A runtime
generation owns its extension processes; idle-time replacement drops the old
generation, and cancellation or timeout kills the affected host without replay.
Custom tools retain ordinary permission/snapshot handling and are denied by the
plan ceiling. Extension hooks can append context and narrow decisions; they
cannot grant permission or replace durable history. See [Extensions](EXTENSIONS.md).

`haarness-profiles` owns portable ZIP validation, configuration capture/restore,
and immutable profile catalog storage. The CLI composes configuration roots;
the server provides authenticated catalog transport without applying profiles.
Profiles never depend on or copy the runtime's conversation databases.

The stable extension surfaces are REST/OpenAPI/SSE, ACP v1 over stdio, MCP,
Agent Skills, agent profiles, and JSON/JSONC configuration (`haarness.json`; an
existing `config.toml` is still read). Workspace Rust crates are
private `0.x` implementation details. Haarness deliberately has no Rust
dynamic-plugin ABI.

Server attachment preserves the same ownership boundary locally and over the
network: `attach` and `run --attach` are strict REST/SSE clients, `/v1/info`
declares the authoritative workspace for that server, and disconnecting a client never
owns or cancels execution. Multiple workspace servers can share one data root,
but each owns a distinct database, discovery record, token, and activity lease.
An explicit listener on a network address requires a password (HTTP Basic) or a
legacy bearer token unless it is started with `--anonymous`, and every listener
keeps same-host browser-Origin checks; client-local PDF/image/audio/video media is
uploaded inline rather than interpreted as a server-side path. Binary `read`,
MCP media, and optional generation results use persisted tool-result attachment
metadata, which the core replays through the provider-neutral message layer.

The ACP adapter is also a strict client of the canonical workspace server, not
a second runtime owner. ACP session IDs are durable Haarness thread IDs;
session updates are projected from the authenticated SSE stream, permission
responses return through the REST API, and closing the editor-side stdio bridge
does not stop the workspace server. The adapter accepts only the workspace it
was launched for, so an editor cannot silently redirect a live server to a
different checkout.
