# TypeScript and JavaScript extensions

How to write, install and configure an extension, the hooks it can use, where it is
discovered, and how its process is trusted and run. For extension authors and for
contributors changing the extension host (`crates/haarness-extensions`).

> **User guide:** [Add a custom tool with an extension](https://haarness.ai/docs/cli/extensions).

Haarness extensions add tools and change chat behavior through a versioned,
typed API. Authors write TypeScript or JavaScript; the application and its
permission, persistence, and lifecycle machinery stay in Rust. There is no
Rust dynamic-library ABI to compile against or keep in sync with releases.

Extensions are optional. They use **Bun**, also used by native TypeScript
workflows. Put Bun on `PATH`, or set `HAARNESS_BUN` to its absolute executable
path. Ordinary CLI/TUI/server use does not require a JavaScript runtime.

## Create your first extension

```sh
haarness extension init ./my-extension
# Edit my-extension/index.ts. api.d.ts supplies editor autocomplete and types.
haarness extension install ./my-extension
haarness extension check
haarness
```

The starter is named after its folder (`./my-extension` becomes `my_extension`;
`--name` chooses another), so it registers `ext__my_extension__greet`. Ask
Haarness to greet someone and it can call the tool. Registered tools also appear
in `/tools`, Desktop's **Settings → Tools** (under the extension's own name) and
the REST tool catalog, with source `extension:my_extension`; ordinary
availability controls and lazy tool loading apply.

`init` writes `index.ts`, `api.d.ts` and a `package.json` whose
`haarness.extension` names `index.ts`. The generated extension needs no SDK
download or build step:

```ts
import type { Extension } from "./api"

export default {
  apiVersion: 1,
  name: "my_extension",
  tools: {
    greet: {
      description: "Greet someone by name",
      parameters: {
        type: "object",
        properties: { name: { type: "string" } },
        required: ["name"],
        additionalProperties: false,
      },
      execute(args: { name: string }) {
        return `Hello, ${args.name}!`
      },
    },
  },
  hooks: {
    chat() {
      return { context: "Use the greeting tool when the user asks for a greeting." }
    },
  },
} satisfies Extension
```

JavaScript authors can omit the type import, argument annotation, and
`satisfies Extension`. A default factory, `export default (context) => ({...})`,
may initialize shared state asynchronously. `context` provides the workspace,
extension source path, and installation options. Tool context also includes
`threadId` and `turnId`.

## Install and manage

```sh
haarness extension install ./my-extension --options '{"greeting":"Hello"}'
haarness extension install ./single-file.ts
haarness extension install npm:haarness-extension-example@1.2.3
haarness extension install npm:@my-team/reviewer@1.0.0
haarness extension list
haarness extension check --verbose # include console output for debugging
haarness extension disable       # disable the complete extension system
haarness extension enable
haarness extension disable my_extension
haarness extension enable my_extension
haarness extension remove my_extension  # unregister it; preserve its files
# Names or the 1-based indices shown by list are accepted.
haarness extension install ./my-extension --project  # this project only (alias: --local)
```

Every `extension` command takes `--project` to manage the project's
`.haarness/extensions.json` instead of the global file. Desktop's
**Settings → Extensions** (shown while Desktop uses a server on this computer)
manages the same lists with a **Global** or **Project** scope: the
**Enable extensions** switch, **Install extension**, a switch per entry and
remove. The TUI has no extension command.

`install` explicitly trusts and validates the code, then enables extensions.
`check` executes initializers and validates API versions, names, hooks, and tool
schemas. `list` never executes extension code. Use install again to validate
changed local sources, or install a newer npm version after removing the old
entry. Duplicate extension names are rejected.

Registry packages install into a private Haarness directory with a Bun
lockfile and exact direct version. Install scripts are disabled. The runtime
does not automatically download missing dependencies. Local extensions can
use their own installed dependencies; npm extensions resolve their installed
dependency tree normally. Registry configuration follows Bun's standard npm
configuration. Haarness does not publish packages or install Bun automatically.

Package authors should identify the entrypoint explicitly:

```json
{
  "name": "haarness-extension-example",
  "version": "1.2.3",
  "type": "module",
  "haarness": { "extension": "src/index.ts" }
}
```

Simple `module`, `main`, or string `exports` entries and `index.ts`, `index.mjs`,
or `index.js` are also accepted. Names of extensions and their tools use 1–24
lowercase letters, digits, or single underscores (double underscores are reserved). Tools are namespaced as
`ext__<extension>__<tool>` and cannot replace a native or MCP tool.

## Discovery and configuration

The global configuration is `extensions.json` in the global configuration folder
(beside `haarness.json` and `config.toml`, normally `~/.config/haarness/`).
`extension list` prints its exact path. It starts disabled:

```json
{
  "enabled": false,
  "timeout_ms": 30000,
  "entries": []
}
```

Project files layer on top: `.haarness/extensions.json` in each folder from the
Git worktree root down to the selected folder, read after the global file (and
skipped when project configuration is turned off). A field a layer sets replaces
the one before it, so changing one timeout keeps the others; an entry for a
source already listed replaces the earlier one; a layer with `enabled: false`
disables its own entries. A project with no file of its own inherits the global
switch and timeouts.

An installed entry has `source` (an absolute path; a relative one is read from
the folder of the file that lists it), `enabled`, and `options` (JSON). `entries` also accepts a bare source string or a
`["<source>", {options}]` tuple; both mean an enabled entry, and the tuple's
second element becomes `context.options`. Plugin and extension declarations in
`haarness.json{,c}` (`extensions: [...]`, path or npm entries of `plugin: [...]`)
join the same registration; see
[plugin and extension declarations](CONFIGURATION-SCOPES.md#plugin-and-extension-declarations).
The timeout applies to initialization and each tool/hook call, including
time queued behind another call to the same extension; the allowed range is
any nonnegative number of milliseconds; `0` disables it. Stage-specific `init_timeout_ms`, `hook_timeout_ms`, and `tool_timeout_ms` override the fallback.

When enabled, Haarness also discovers `.ts`, `.mts`, `.js`, and `.mjs` entry
files directly inside:

- `extensions/` beside the global configuration;
- `.haarness/extensions/` beside each project `extensions.json` layer, from the
  Git worktree root down to the selected folder (unless project configuration
  is disabled);
- `extensions/` inside enabled, installed plugin bundles.

Type declaration files (`.d.ts`, `.d.mts`) are excluded. Put helper modules in subdirectories so
they are not treated as additional entrypoints. Extension discovery follows the extension enable setting. Project sources participate by default; `--disable-project-config` or `HAARNESS_DISABLE_PROJECT_CONFIG=1` excludes them. To disable one
discovered entry, put its absolute source path in `entries` with `enabled:false`,
or disable/remove its enclosing plugin.

Source/config changes use the existing idle-time reload mechanism. Active
turns keep their loaded extension generation. Invalid replacement code leaves
the previous generation running and reports a reload failure. A successful
reload resets extension process state; durable state belongs in explicitly
managed files or another service. Keep mutable state outside watched source
directories to avoid triggering reloads. Initializers should avoid side effects:
validation and candidate reloads can initialize code before it becomes active.

## Hooks

All hooks are optional and may return promises. Names and payload shapes
correspond to runtime lifecycle events; `threadId` stands in for `sessionID`.

| Hook | Input | Supported result |
| --- | --- | --- |
| `prompt` | `threadId`, `prompt` | `{context}` adds system context for this turn; `{block}` rejects the prompt |
| `chat` | `threadId`, `model`, `system`, `reasoningEffort` | `{context}` appends system context; `{reasoningEffort}` overrides that request's effort |
| `chat.params` | `threadId`, `model`, `temperature`, `topP`, `topK`, `maxOutputTokens`, `reasoningEffort`, `options` | Sampling limits for this request: `{temperature, topP, topK, maxOutputTokens}` set a value, `null` clears it; `{reasoningEffort}`; `{options}` merges extra JSON body fields into OpenAI-compatible requests |
| `chat.headers` | `threadId`, `model`, `headers` | `{headers}` adds HTTP headers to the provider request |
| `tool.before` | `threadId`, `tool`, `arguments` | `{arguments}` replaces input before permission evaluation; `{context}` adds context; `{block}` denies execution. Runs for the `question` tool too, where `{block}` stops the question and `{arguments}` with `answers` answers it without asking (see [lifecycle hooks](CONFIGURATION-SCOPES.md#lifecycle-hooks)) |
| `tool.after` | Same identity/input plus `output`, `isError` | `{context}` adds context to the tool result; for `question`, `output` is the answer; after a call that failed once its tool ran, `isError` is true, `output` is the error, and the context follows that error |
| `tool.definition` | `toolID`, `description`, `parameters` | `{description}`, `{parameters}` rewrite the model-facing definition of that tool |
| `permission.ask` | `threadId`, `permission`, `patterns`, `detail` | `{status: "allow" \| "deny" \| "ask"}` answers the prompt before the user sees it; `{reason}` explains a denial |
| `command.execute.before` | `threadId`, `command`, `arguments` | `{block}` aborts the slash command before its template expands; `{context}` joins the turn's hook context |
| `shell.env` | `threadId`, `cwd` | `{env}` adds environment variables to that bash tool invocation |
| `session.start` | `threadId`, `agent`, `directory` | Observation; no result |
| `session.end` | `threadId`, `reason`, `directory` | Observation; no result |
| `session.compacted` | `threadId`, `trigger`, `compaction` | After a committed manual or automatic compaction; no result |
| `chat.turn.failed` | `threadId`, `turnId`, `detail` | Terminal failure after recovery, excluding cancellation; `detail` holds `error` (`incomplete`, `content_filter`, `provider`, `tool` or `other`), `error_type` (what happened, as Claude Code names it: `authentication_failed`, `rate_limit`, `overloaded`, `server_error`, `invalid_request` or `unknown` for a provider error, otherwise the same as `error`, and `unknown` for `other`), `error_details`, `provider_id` and `model_id`; no result |
| `chat.turn.end` | `threadId`, `turnId`, `agent`, `provider`, `model`, `finishReason` | Observation; no result |
| `config` | Effective configuration summary (`workspace`, `provider`, `model`, `permission`, `mcp`, `agents`, `commands`, `execution`, …) | Observation; no result |
| `stop` | `threadId` | Completion notification; no result |
| `event` | `type` plus the corresponding hook input | Observes every hook above, before the specific hook |

Where each hook fires:

- `session.start` when a chat is created, but not a chat started inside
  another (a task subagent, a workflow's thread or agent); `session.end` when a thread is
  archived (Haarness has no thread deletion). `chat.turn.end` follows `stop`
  for every completed root turn; token and cost totals are not part of the
  payload yet.
- `chat`, `chat.params` and `chat.headers` run in that order on every
  provider request, once per extension. Temperature, top-p, top-k and the
  output-token limit reach the OpenAI-compatible, Anthropic, Gemini and OpenAI
  transports where the wire protocol has the field (Anthropic keeps
  `max_tokens` above an active thinking budget); `options` only reaches
  OpenAI-compatible requests; headers reach those four HTTP transports and are
  ignored by signed transports such as Bedrock.
- `permission.ask` runs before the user is prompted and before command
  `PermissionRequest` hooks; `allow` and `deny` resolve the prompt, `ask` or
  no result keeps it, and a thrown error denies. A configured `deny` rule is
  never reopened. `patterns` are what the prompt asks about and `detail` is
  the tool's arguments. When a prompt asks about the file a path really opens
  (a link `notes.txt -> .env` asks for `.env`), that file is asked about in a
  call of its own, with `patterns: [".env"]` and `detail.path` set to it, so
  an extension judges the file the tool opens rather than the link. The
  prompt is answered only when every call allows it; a `deny` in any call
  refuses it.
- `command.execute.before` runs when a slash command turn starts, before its
  template expands.
- `shell.env` runs for each `bash` tool call, after permission evaluation;
  later extensions override earlier ones and invalid variable names are
  dropped. A command the agent runs is still marked as an agent's: `AGENT`,
  `HAARNESS` and `HAARNESS_SESSION_ID` win over an extension's values, and an
  `AI_AGENT` it sets is kept (see
  [Permissions and execution](PERMISSIONS-AND-EXECUTION.md#configure-execution-budgets)).
- `tool.definition` runs once per tool when the registry is composed (startup
  and hot reload), for native, MCP and extension tools known at that time; an
  error there keeps that configuration from loading. A tool listed later, such
  as one of an MCP server that was still starting, gets the hook before the
  next turn's first request, where an error only logs a warning and the tool
  keeps its own definition. Arguments are still validated against the tool's
  own contract, so a rewritten schema must stay compatible with it.
- `config` runs once per loaded generation with a summary of the effective
  configuration; credentials are never included.

The event hook is a lifecycle stream, not a subscription to every REST/SSE
event or individual token. Hooks cannot grant permissions beyond an open
prompt, replace tool execution contracts, or rewrite durable history. An
existing hook denial wins. An exception in a prompt, chat, command, permission
or pre-tool hook blocks that operation; a post-tool or observation error is
reported without pretending the already-completed effect was undone. A normal
tool exception is returned as a tool error and does not kill the host.

Tools return a string or `{text, metadata}`. Rust validates arguments against
the tool's JSON Schema before execution. Every extension tool is conservatively
classified as mutating, participates in snapshots, and is unavailable in plan
mode. Permission rules use the namespaced tool name, for example:

```json
{"permission":{"ext__my_extension__greet":"ask"}}
```

## Trust and process lifetime

Extensions are **trusted executable code**, with the user's filesystem,
network, and process access. They run out of process to isolate crashes and
support termination; this is not an OS sandbox. Native filesystem/network
calls made inside an extension are not individually intercepted by tool
permissions. Choose MCP when you want a separately managed tool server.

Because extension code can answer approvals, an agent's `edit`, `write`,
`multiedit` or `apply_patch` of `.haarness/extensions.json`, any file under
`.haarness/extensions/`, or the global folder's `extensions.json` and
`extensions/` (also where a link leads) always asks a person first; see
[Haarness's own configuration files](PERMISSIONS-AND-EXECUTION.md#haarnesss-own-configuration-files).

Rust serializes calls within one extension process; different extensions have
independent processes. Cancelling or timing out an in-flight call terminates its host. Cancelling a
queued call leaves the call ahead of it running. Failed calls are never
automatically replayed. Reload the source/config or
restart the server to replace a terminated host. Unix termination also kills
the extension's process group; on Windows the direct host is terminated, and
authors must manage any detached child processes they launch.

Protocol input/output defaults to 2 MiB per message, configurable with positive `payload_max_bytes`. Do not write directly
to stdout; it is the transport channel. Console messages are diverted off the
protocol channel and discarded during normal operation. Use
`haarness extension check --verbose` to display them while validating. Surface useful operational details through
tool results or explicit application-owned logging. Project Bun configuration,
automatic `.env` loading, and environment preload options do not control the
extension host.

## Verification

```sh
cargo test -p haarness-extensions
cargo build -p haarness-cli
python3 scripts/test-extensions.py
```

The integration test uses private workspaces, a local package registry and
provider HTTP fixture, real Bun processes, the running Haarness server, and a
tmux-rendered TUI. It requires Bun, Python 3, and tmux. It does not use paid
models or modify the user's installed plugins.

See [extension budgets](PERMISSIONS-AND-EXECUTION.md#extension-budgets) and [skills and plan mode](PERMISSIONS-AND-EXECUTION.md#skills-and-plan-mode) for scoped configuration examples and plan-mode observation.
