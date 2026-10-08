# Configuration and resource scopes

Haarness separates user configuration, project overrides, persistent runtime data, transient cache, and application state. Desktop, CLI, TUI, daemon, and management tools resolve these locations through the CLI composition root; Electron does not own a separate agent configuration.

## Locations

| Content | Global | Project |
| --- | --- | --- |
| Configuration, permissions, provider definitions, MCP, hooks, tools, compaction and language settings | `~/.config/haarness/haarness.json` or `.jsonc` | `haarness.json{,c}` and `.haarness/haarness.json{,c}` |
| Agents and commands | `~/.config/haarness/{agents,commands}/` | `.haarness/{agents,commands}/` |
| Skills | `~/.config/haarness/skills/` | `.haarness/skills/` |
| Local plugin bundles | `~/.config/haarness/plugins/local/` | `.haarness/plugins/local/` |
| Extension registration and enablement | `~/.config/haarness/extensions.json` | `.haarness/extensions.json` |
| Discoverable extension source | `~/.config/haarness/extensions/` | `.haarness/extensions/` |
| Provider credentials | `~/.local/share/haarness/auth.json` | Never stored in project configuration by credential management |
| Conversations, project registrations, memory, schedules, plugin install records | `~/.local/share/haarness/` | Runtime records are keyed by workspace identity |
| Daemon discovery, leases and other disposable caches | `~/.cache/haarness/` | Workspace-specific subdirectories |
| Application state plane | `~/.local/state/haarness/` | Presentation state remains separate from agent configuration |

`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` replace the corresponding base directories, including on macOS. Absolute `HAARNESS_HOME` puts the four planes beneath one root (`config`, `data`, `cache`, `state`). Explicit `--data-dir` / `HAARNESS_DATA_DIR` remains supported for existing isolated installations and test fixtures. Chromium window preferences, encrypted remote connection credentials and browser storage remain in Electron's native user-data directory; they are not project or agent configuration.

## Precedence and management

Global settings load first, then project layers from the Git worktree root down to the selected workspace. Outside Git, the selected workspace is the boundary. Within each directory, Cloud session defaults in `.haarness/cloud/haarness.json` load first; regular project files can override them. Attached Cloud skill assets live in `.haarness/cloud/skills/`. This layer is also skipped by `--disable-project-config`. Within each directory, hidden `.haarness` configuration overrides root configuration. JSON overrides the matching JSONC file. Native Haarness resources override compatible resources; project resources override global resources with the same name. Singular `agent`, `command` and `skill` directories remain readable alongside plural directories.

Legacy `config.toml` remains readable and existing installations keep that format when managed in place. New provider/MCP settings use the scoped `haarness.json` file. JSONC management preserves unrelated comments and fields. Provider credentials remain global regardless of the scope of the provider definition. Project permission rules follow global rules; agent restrictions and workspace-specific runtime approvals retain the engine's existing enforcement semantics.

Managed Chat Completions gateways can set `providers.<id>.server_output_limit = true` to let the server choose a budget-aware default output allowance. Ordinary providers keep their existing client defaults. An explicit request-hook output limit still reaches the server unchanged. Cloud session bootstrap enables this for its router provider.

Creating managed agents, commands, skills and plugins defaults to Global. Explicit Project scope writes within `.haarness`. Reads and edits resolve project before global when the scope is omitted; explicitly scoped operations only modify that scope. Extension settings now expose the same Global/Project choice. A project can override an inherited source without modifying another project's extensions. Executable project configuration continues to respect the existing project setting; the desktop enables it on opening a workspace.

Explicitly supported compatibility resources remain readable. These are imports, not Haarness-owned files: management does not rewrite the other application's credential/configuration stores. Plugin install records and fetched bundles remain runtime data, while user-authored local bundles belong to configuration. Profiles include portable resources from either scope and exclude credential stores and conversation databases.

## Instruction files

`AGENTS.md` in the global configuration directory loads first, then `AGENTS.md` from the Git worktree root down to the selected workspace. Each file may `@import` other files: bare `@docs/rules.md`, or `@"path with spaces.md"`, `@'path.md'` and `@<path.md>` for paths containing spaces; references inside fenced code blocks and inline code spans stay literal. Imports follow at most five hops, twenty files and 200 KiB per assembly, 40 KiB per file; project files cannot import from outside the workspace.

The `instructions` key in `haarness.json{,c}` and its legacy filename aliases appends more sources, in layer order:

```jsonc
{
  "instructions": [
    "docs/*.md",                        // glob, matched in the workspace and each ancestor up to the worktree root
    "~/shared/rules.md",                // home-relative
    "/opt/team/standards.md",           // absolute (globs allowed)
    "https://example.test/rules.md"     // fetched with a 10 s timeout, 200 KiB cap; failures are logged and skipped
  ]
}
```

Files under the global configuration directory or the supported home resource roots may import from outside the workspace; everything else is bounded like project `AGENTS.md`. Relative entries are ignored when project configuration is disabled.

When a `read` returns a file inside a workspace subdirectory, every `AGENTS.md` between that directory and the workspace root that the system prompt does not already carry is appended to the tool result, once per thread (at most twenty files, 40 KiB each).

## Claude Code compatibility

Claude Code home and project files are never read unless a category is enabled:

```jsonc
{
  "compatibility": {
    "claude_code": {
      "instructions": true, // CLAUDE.md, .claude/CLAUDE.md, CONTEXT.md, *.local.md siblings and ~/.claude/CLAUDE.md
      "agents": true,       // .claude/agents/*.md (project walk-up) and ~/.claude/agents
      "commands": true,     // .claude/commands and ~/.claude/commands
      "skills": true        // .claude/skills and ~/.claude/skills
      // "enabled": false   // master switch: turns every category off
    }
  }
}
```

The same keys are accepted in TOML under `[compatibility.claude_code]`. With `instructions` on, each directory contributes the first family member that exists (`AGENTS.md`, then `CLAUDE.md`, `.claude/CLAUDE.md`, `CONTEXT.md`) together with its `.local.md` sibling; `~/.claude/CLAUDE.md` follows the global `AGENTS.md`, and lazy discovery on `read` includes the same family. Claude agent frontmatter is normalised: `tools` may be a comma-separated string or a list, `allowed-tools` stands in for a missing `tools`, colour names map onto the theme names (`red` → `error`, `green` → `success`, `yellow`/`orange` → `warning`, `blue`/`cyan` → `info`, `pink`/`magenta`/`purple` → `accent`, `gray`/`grey` → `secondary`), `model: inherit` means no pin, and the bare `sonnet`/`opus`/`haiku` aliases resolve at dispatch time. Claude resources load before native ones, so a Haarness command, agent or skill with the same name wins. Every category is off by default, so existing installations behave exactly as before.

## Agent and command metadata

Markdown agents and `agent` entries in JSON accept the presentation and limit fields: `hidden` (kept out of pickers and `@` autocomplete), `color` (`#rrggbb` or a theme name), `steps`/`maxSteps` (provider/tool rounds before a final tool-free reply; overrides `tools.max_rounds` for that agent), `disable` (drops the agent, including one defined by an earlier layer), and `temperature`, `top_p`, `variant` (parsed and stored; not yet applied to provider requests). Command frontmatter and JSON `command` entries accept `subtask: true` as an alias of `context: fork`.

Inside a command template, `@path` references that name a workspace file are appended as bounded snapshots (128 KiB each, twenty per command); `@dir/` appends a one-level listing (200 entries); an `@name` that matches a visible subagent-mode agent and no file becomes the same delegation hint the TUI attaches. References inside code stay literal and paths outside the workspace are ignored unless written with `~/`.

## Skills from paths and URLs

```jsonc
{
  "skills": {
    "paths": ["./team-skills", "~/skills"],
    "urls": ["https://example.test/skills/"]
  }
}
```

`paths` are additional skill roots (`~/` and workspace-relative paths allowed; missing folders are skipped). Each `urls` entry names an index at `<url>/index.json` (or the `index.json` itself) of the shape `{"skills":[{"name":"…","description":"…","files":["SKILL.md", …]}]}`, with files fetched from `<url>/<name>/<file>`; an entry may instead point straight at its file with `"url"`. Fetches are best effort with a 10 s timeout; downloaded skills are cached under `~/.local/share/haarness/remote-skills/<host>/<name>/` and never refetched while present. Remote and path skills are listed after every native root, so a local skill with the same name wins.

## Message delivery preference

`input.delivery` in native Haarness JSON/JSONC files accepts `"steer"` (the built-in default) or `"queue"`. Desktop General settings and `/input-mode` in both clients share this value on the selected server. `/input-mode` toggles only the current project, writing its `.haarness/haarness.json{,c}` without modifying an inherited ancestor. `/input-mode reset` removes the current directory's override and restores inheritance. This non-executable preference applies on the next submission, including during active work. See [Steer and Queue](INPUT-QUEUE.md) for commands and API examples.

## Interface language

The interface defaults to English on both clients, regardless of OS or terminal language. Use `/language` in either client, `/language en` to restore English, or Desktop Settings → General → Language. This is a saved client presentation preference, separate from project agent configuration: the TUI uses its saved state (then explicit `[tui].language` in application `config.toml`), and Desktop uses its local preferences. See [supported languages and translation behavior](../locales/README.md).

## Existing installations

On first use of the new layout, configuration and managed resources are copied from the former OS-specific directories without overwriting existing destinations. Credentials are adopted into the data plane. Migration markers prevent deleted settings or credentials from reappearing on the next launch. Source files are retained.

Existing data and cache directories are linked at their new canonical locations (a directory junction on Windows), rather than copying live SQLite databases, WAL files or daemon locks. This retains their identity and all concurrent writes; old absolute paths embedded in plugin registries stay valid. Fresh installations use ordinary directories. The compatibility links deliberately keep existing storage in place; no background data deletion is performed.

## Execution and remembered permissions

The native `execution` object supports configurable shell/Code Mode deadlines, media size, read concurrency, subagent concurrency/depth and exact observational extension exceptions for plan mode. Project overrides merge per field. See [Permissions and execution settings](PERMISSIONS-AND-EXECUTION.md) for defaults and examples, including explicit repository approvals shared across linked worktrees.

## Plugin and extension declarations

`haarness.json{,c}` and its legacy filename aliases accept the plugin and extension registration keys. Nothing is downloaded or cloned while configuration loads; the keys map onto Haarness's existing registration paths:

```jsonc
{
  "extensions": [
    "./extensions/reviewer",                      // path, relative to this file (file:// and ~/ also accepted)
    ["@team/reviewer@1.0.0", { "strict": true }]  // [spec, options]: options become context.options
  ],
  "plugins": [
    "reviewer@internal",                          // name@marketplace (or marketplace/name)
    { "spec": "linter@internal", "enabled": false }
  ],
  "plugin": ["example-notify", "docs@internal"]  // npm/path entries are extensions, name@marketplace entries are plugins
}
```

- **Extensions** (`extensions`, and `plugin` entries that are paths, `file://` URLs, `npm:` specs, scoped or versioned npm names) register through the same path as `extensions.json`. A path resolves relative to the declaring file and must name an extension file or package. An npm spec resolves to the newest package that `haarness extension install npm:<spec>` already installed; otherwise it is reported at startup as pending (`extension package X is declared in <file> but not installed; run haarness extension install npm:X`) because installing executes registry code and needs the network. Declared entries are enabled; an `extensions.json` entry for the same source wins, so `enabled: false` there still disables it. Declarations turn the extension system on unless an `extensions.json` file exists to say otherwise (`haarness extension disable` keeps winning). Project declarations require project trust, like every executable project setting.
- **Plugins** (`plugins`, and `plugin` entries of the form `name@marketplace` or `marketplace/name`) name marketplace bundles managed by `haarness plugin`. A declared plugin that `installed.json` does not contain is a pending install: it appears in `haarness plugin list` under "Declared in configuration but not installed" and in a startup notice with the exact `haarness plugin install <spec>` to run. `{ "spec": ..., "enabled": false }` hides an installed plugin from discovery (hooks, commands, agents, skills and bundled extensions) without rewriting the install record, and `haarness plugin list` shows it disabled.
- Entries that cannot be parsed are skipped with a notice naming the file and index; they never invalidate the configuration file.

The parser and mapping live in `crates/haarness-cli/src/config_specs.rs`; `extensions::sources` and `plugins::all_enabled_roots` consume them.

## Lifecycle hooks

The `hooks` object in any configuration layer (and `hooks/hooks.json` in a plugin bundle) runs shell commands at the lifecycle events listed below. Each handler receives one JSON document on stdin with `hook_event_name`, `cwd` and, where a thread exists, `session_id`; exit code 2 or a JSON `{"decision": "block"}` answer blocks the events that can be blocked.

| Event | When | Payload beyond the common fields | Matcher compares |
| --- | --- | --- | --- |
| `SessionStart` | a thread is created | `source` (`startup`), `agent` | `source` |
| `UserPromptSubmit` | a root turn starts | `prompt` | — |
| `PreToolUse` / `PermissionRequest` | before a tool runs / before the user is prompted | `tool_name`, `tool_input` | tool name (Claude aliases accepted) |
| `PostToolUse` / `PostToolUseFailure` | after a tool finishes | `tool_name`, `tool_input`, `tool_response`, `error` | tool name |
| `FileChanged` | after a successful `write`, `edit`, `multiedit` or notebook edit | `tool_name`, `tool_input`, `file_path` | tool name |
| `Stop` | a root turn completed | — | — |
| `PostCompact` | after a compaction commits | `trigger`, `compaction` | `manual` or `automatic` |
| `StopFailure` | once after terminal failure and exhausted recovery | `error`, `error_details`, `provider_id`, `model_id` | `incomplete`, `content_filter`, `provider`, `tool`, `other` |
| `TurnEnd` | after `Stop`, once per completed root turn | `turn_id`, `agent`, `provider`, `model`, `finish_reason` | — |
| `Notification` | a permission prompt is waiting (`permission_prompt`) or a turn completed (`turn_completed`) | `notification_type`, `message` | `notification_type` |
| `SessionEnd` | a thread is archived | `reason` (`archived`) | `reason` |
| `ConfigChange` | a hot-reloaded configuration generation became active | `source` (`reload`); no `session_id` | — |

Any other non-zero exit is a non-blocking error: the action continues and the daemon log records one `hook command failed` warning with the event, the command's program name, its exit code or signal and the start of its redacted stderr. Every block is logged once at INFO as `hook blocked`. The configured command line is never logged; see [observability](OBSERVABILITY.md#diagnostic-log-lines).

`SessionStart`, `SessionEnd`, `TurnEnd`, `Notification`, `FileChanged` and `ConfigChange` are observational: their output is logged, never applied. The desktop hook editor accepts these lifecycle events. `PostCompact` and `StopFailure` are observational: hook output never changes the compaction or retries a failed response. Cancellation does not emit `StopFailure`. JavaScript extensions receive the matching `session.start`, `session.end`, `session.compacted`, `chat.turn.failed`, `chat.turn.end` and `config` hooks; see [extensions](EXTENSIONS.md#hooks).

### Recovery and language-server limits

Incomplete provider streams retain partial text and usage and receive at most three recovery attempts per run. Empty responses retry the request; responses with partial output continue from durable history. Tools from an incomplete response are closed with an error instead of being executed, and completed earlier tool results remain in history. Incomplete-response recovery does not retry explicit provider errors or policy blocks. Every attempt contributes to usage accounting; context size remains based on the current conversation.

Language servers stop after ten idle minutes by default. Configure `[lsp] idle_timeout_ms` and `max_clients` (default 6) to adjust retention per workspace daemon. Active operations, including diagnostic waits, are protected. At capacity, the oldest idle client is stopped; when all clients are busy, optional language work reports a capacity error. Reaped clients restart on demand without a failure backoff.

A subagent whose configured model is absent from its authenticated provider's catalog uses the parent's model and includes a substitution notice in foreground and background task results. An explicit model argument takes precedence. Unknown providers and authentication/configuration errors are reported instead of triggering fallback.
