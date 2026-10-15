# Configuration and resource scopes

> **User guides:** [Configure Haarness](https://haarness.ai/docs/cli/configuration), [Data and storage](https://haarness.ai/docs/security/data-and-storage). This page has the engineering detail.

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
| Chrome settings (`browser.chrome`) | `~/.config/haarness/haarness.json` or `.jsonc` | Can only tighten the global value; see below |
| Haarness for Chrome: host wrapper, copied binary, unpacked extension, opt-out marker (and the host manifest on Windows) | `~/.local/share/haarness/browser/chrome/` | — |
| Chrome upload staging | `~/.cache/haarness/browser/uploads/` | — |
| Native messaging host registration | Each browser's `NativeMessagingHosts` folder (macOS, Linux) or `HKCU` registry key (Windows) | — |

`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` replace the corresponding base directories, including on macOS. Absolute `HAARNESS_HOME` puts the four planes beneath one root (`config`, `data`, `cache`, `state`). Explicit `--data-dir` / `HAARNESS_DATA_DIR` remains supported for existing isolated installations and test fixtures. Chromium window preferences, encrypted remote connection credentials and browser storage remain in Electron's native user-data directory; they are not project or agent configuration. Outside Haarness's own folders, `haarness browser setup` adds only the `ai.haarness.browser.json` host manifest to each browser's user-data folder (or a registry value on Windows); the extension keeps its own state in Chrome's extension storage. The wrapper it writes exports the absolute `HAARNESS_HOME`, `HAARNESS_DATA_DIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` values in effect at setup, so a Chrome started from the Dock reaches the same roots. With a separate data folder (`--data-dir`, `HAARNESS_DATA_DIR` or `HAARNESS_HOME`), setup registers with the user's browsers only when given `--use-isolated-data-for-chrome`; test runs point it at a test profile with `--manifest-dir` or `HAARNESS_BROWSER_MANIFEST_DIR` instead. See [Haarness for Chrome](CHROME-EXTENSION.md#setup-and-data-layout).

## Precedence and management

Global settings load first, then project layers from the Git worktree root down to the selected workspace. Outside Git, the selected workspace is the boundary. Within each directory, Cloud session defaults in `.haarness/cloud/haarness.json` load first; regular project files can override them. Attached Cloud skill assets live in `.haarness/cloud/skills/`. This layer is also skipped by `--disable-project-config`. Within each directory, hidden `.haarness` configuration overrides root configuration. JSON overrides the matching JSONC file. Native Haarness resources override compatible resources; project resources override global resources with the same name. Singular `agent`, `command` and `skill` directories remain readable alongside plural directories.

Legacy `config.toml` remains readable and existing installations keep that format when managed in place. New provider/MCP settings use the scoped `haarness.json` file. JSONC management preserves unrelated comments and fields. Provider credentials remain global regardless of the scope of the provider definition, so a project cannot change the `base_url` of a built-in provider, a preset, or a provider defined in the global configuration: requests to that id carry the credential stored for it. Such a project entry is ignored with a notice (`Provider anthropic: ignored the project's base_url …`); give a project's own endpoint its own provider id. `haarness provider add` and the custom-provider forms in Desktop and the TUI refuse built-in ids for the same reason. Project permission rules follow global rules; agent restrictions and workspace-specific runtime approvals retain the engine's existing enforcement semantics. Chrome settings are the exception to project overrides: `browser.chrome` is read from the global layer, and a project layer can only turn Chrome off or make its site decisions stricter (allow → ask → deny); any deny in a layer's `browser.policy` also applies to Chrome, and `browser.policy.enabled: false` in any layer turns Chrome off. Desktop and the TUI always save Chrome settings globally. See [Chrome settings](BROWSER.md#chrome-settings).

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

The same keys are accepted in TOML under `[compatibility.claude_code]`. With `instructions` on, each directory contributes the first family member that exists (`AGENTS.md`, then `CLAUDE.md`, `.claude/CLAUDE.md`, `CONTEXT.md`) together with its `.local.md` sibling; `~/.claude/CLAUDE.md` follows the global `AGENTS.md`, and lazy discovery on `read` includes the same family. Claude agent frontmatter is normalised: `tools` may be a comma-separated string or a list, `allowed-tools` stands in for a missing `tools`, colour names map onto the theme names (`red` → `error`, `green` → `success`, `yellow`/`orange` → `warning`, `blue`/`cyan` → `info`, `pink`/`magenta`/`purple` → `accent`, `gray`/`grey` → `secondary`), `model: inherit` means no pin, the `sonnet`/`opus`/`haiku`/`fable` families resolve at dispatch time to the newest release a provider lists (by version, so Sonnet 5.5 wins over Sonnet 5 whether ids end in a Bedrock `-v1:0`, a Vertex `@date` or a snapshot date), and a bare Claude id such as `claude-sonnet-5-5` resolves to that release in the provider's own spelling (`anthropic/claude-sonnet-5.5` on Haarness Cloud, `us.anthropic.claude-sonnet-5-5-v1:0` on Bedrock). Both prefer the chat's provider, then signed-in providers. Claude resources load before native ones, so a Haarness command, agent or skill with the same name wins. Every category is off by default, so existing installations behave exactly as before.

## Agent and command metadata

Markdown agents and `agent` entries in JSON accept the presentation and limit fields: `hidden` (kept out of pickers and `@` autocomplete), `color` (`#rrggbb` or a theme name), `steps`/`maxSteps` (provider/tool rounds before a final tool-free reply; overrides `tools.max_rounds` for that agent), `disable` (drops the agent, including one defined by an earlier layer), and `temperature`, `top_p`, `variant` (parsed and stored; not yet applied to provider requests). Command frontmatter and JSON `command` entries accept `subtask: true` as an alias of `context: fork`.

A turn reads its agent's definition once, when it is admitted (a delegated child: when it is dispatched), and keeps it until it ends: prompt, model, `steps`, `tools` and permission rules. Editing, renaming or deleting the agent from its file, the management tool, Desktop or the TUI changes the next turn, never a running one, so a running `reviewer` whose rules deny edits keeps denying them after its file is deleted. Work a running turn starts (a workflow agent, a direct tool on its child) takes its ceiling from that same definition. Edits to agent files made outside Haarness apply to new turns without waiting for other chats to go idle; see [hot reload](API.md).

Frontmatter that Claude Code accepts but YAML rejects is read the way Claude Code reads it: a top-level value with a colon and space inside it, such as `allowed-tools: Bash(git commit: *), Read`, is text, and so is an `argument-hint` made of bracketed placeholders, such as `[filename] [format]`. A value that opens a list or map (`tools: [Read, Grep]`), an alias, an anchor or a tag must still be valid YAML, so a malformed policy list fails instead of turning into text. A command, agent or skill whose frontmatter still does not parse is skipped and recorded in the daemon log as `config: skipped <command|agent|skill> <path>: <error>`; editing or customizing it from Haarness fails with that error and leaves the file unchanged.

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
| `PreToolUse` / `PermissionRequest` | before a tool runs / before the user is prompted | `tool_name`, `tool_input`; `PermissionRequest` adds `permission_patterns` | tool name (Claude aliases accepted) |
| `PostToolUse` / `PostToolUseFailure` | after a tool finishes | `tool_name`, `tool_input`, `tool_response`, `error` | tool name |
| `FileChanged` | after a successful `write`, `edit`, `multiedit` or notebook edit | `tool_name`, `tool_input`, `file_path` | tool name |
| `Stop` | a root turn completed | — | — |
| `PostCompact` | after a compaction commits | `trigger`, `compaction` | `manual` or `automatic` |
| `StopFailure` | once after terminal failure and exhausted recovery | `error`, `error_details`, `provider_id`, `model_id` | `incomplete`, `content_filter`, `provider`, `tool`, `other` |
| `TurnEnd` | after `Stop`, once per completed root turn | `turn_id`, `agent`, `provider`, `model`, `finish_reason` | — |
| `Notification` | a permission prompt is waiting (`permission_prompt`) or a turn completed (`turn_completed`) | `notification_type`, `message` | `notification_type` |
| `SessionEnd` | a thread is archived | `reason` (`archived`) | `reason` |
| `ConfigChange` | a hot-reloaded configuration generation became active | `source` (`reload`); no `session_id` | — |

`additionalContext` (top level or in `hookSpecificOutput`) from `UserPromptSubmit` reaches the model with that prompt: it is stored with the turn as a hidden message after the prompt, and later turns replay it as history. It is not added to the system prompt, so a hook that answers differently every turn does not rewrite the provider's cached prompt.

`tool_input` carries the tool's own arguments (`path`, `command`, …). For `PermissionRequest`, `tool_name` is the permission asked (`read`, `edit`, `bash`, `external_directory`, …) and `permission_patterns` what the prompt asks about. A file tool also asks about the file it really opens when links or `..` lead elsewhere, and the hook is asked about that file in a run of its own, with `tool_input.path` set to it: for a link `notes.txt -> .env`, the hook sees `.env`, not `notes.txt`. The prompt is answered only when every run allows it, and a deny in any run refuses it. For the same reason a `PreToolUse` allow, given for the path the tool names, never answers an ask about where that path leads. See [where a file really is](PERMISSIONS-AND-EXECUTION.md#secret-files).

Any other non-zero exit is a non-blocking error: the action continues and the daemon log records one `hook command failed` warning with the event, the command's program name, its exit code or signal and the start of its redacted stderr. Every block is logged once at INFO as `hook blocked`. The configured command line is never logged; see [observability](OBSERVABILITY.md#diagnostic-log-lines).

`SessionStart`, `SessionEnd`, `TurnEnd`, `Notification`, `FileChanged` and `ConfigChange` are observational: their output is logged, never applied. The desktop hook editor accepts these lifecycle events. `PostCompact` and `StopFailure` are observational: hook output never changes the compaction or retries a failed response. Cancellation does not emit `StopFailure`. JavaScript extensions receive the matching `session.start`, `session.end`, `session.compacted`, `chat.turn.failed`, `chat.turn.end` and `config` hooks; see [extensions](EXTENSIONS.md#hooks).

### Recovery and language-server limits

A provider that falls silent does not hold a turn: when a response sends nothing for five minutes (no text, and none of the keep-alive pings or comments providers send while they think), Haarness closes its request, so the provider stops generating, and says the provider sent nothing for 5 minutes, never that a connection was reset. The same limit covers the wait for the response to begin. Before any output the request is retried like an overloaded provider (up to four times, after 2, 4, 8 and 16 seconds); after partial output it is recovered like an incomplete stream below, continuing from what arrived. `/btw`, compaction summaries and titles have the same limit and fail instead of retrying. Set `providers.<id>.stream_idle_timeout` in seconds (`0` never gives up); a section without a `base_url` that only sets timeouts tunes a built-in provider or a detected local runtime without redefining it, and in a later configuration layer it tunes the earlier definition instead of replacing it. A provider on this machine (`localhost`, `127.0.0.0/8`, `::1`) waits as long as it takes unless configured: a local model sends nothing while it loads and reads a long prompt, which on a laptop can take far longer than five minutes, and a local server that dies closes its connection at once, so there is no silent network path to guard against. Amazon Bedrock's Converse answers in one piece, so silence until then is normal; it has `providers.<id>.request_timeout` instead, a limit on the whole answer (900 seconds by default, `0` waits as long as it takes), and reaching it fails the turn without a retry, since the answer may simply be long.

Incomplete provider streams retain partial text and usage and receive at most three recovery attempts per run. Empty responses retry the request; responses with partial output continue from durable history. Tools from an incomplete response are closed with an error instead of being executed, and completed earlier tool results remain in history. Incomplete-response recovery does not retry explicit provider errors or policy blocks. A response the provider's safety check stops (a content filter, a Bedrock guardrail, or Anthropic's `refusal` stop reason) fails the turn with code `provider_blocked` and says to send a follow-up message, rephrase the request or choose another model; text that streamed before the stop stays in the chat, and `StopFailure` hooks see it as `content_filter`. Every attempt contributes to usage accounting; context size remains based on the current conversation.

Gemini drops a function call it cannot parse, or one naming a tool it was not offered, and ends the response with `MALFORMED_FUNCTION_CALL` or `UNEXPECTED_TOOL_CALL`. Those responses count as incomplete rather than finished with an empty answer: the next request asks the model to continue, so it can issue the call again. Valid calls that arrived before the dropped one run as usual instead of being closed with an error, and only a response with no valid call uses up one of the three recovery attempts. `HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-gemini-recovery.py` drives both cases, a discovered Gemini 4 model's thinking level and the region refusal below through a real daemon and TUI against a mock Gemini API.

A provider that refuses requests from this country or region (OpenAI's `unsupported_country_region_territory`, Gemini's "User location is not supported", Bedrock's "unsupported countries, regions, or territories") fails the turn at once, without retries, and says to choose a model from another provider.

Language servers stop after ten idle minutes by default. Configure `[lsp] idle_timeout_ms` and `max_clients` (default 6) to adjust retention per workspace daemon. Active operations, including diagnostic waits, are protected. At capacity, the oldest idle client is stopped; when all clients are busy, optional language work reports a capacity error. Reaped clients restart on demand without a failure backoff.

A subagent whose configured model is absent from its authenticated provider's catalog uses the parent's model and includes a substitution notice in foreground and background task results. A family or bare Claude id that no configured provider lists counts as absent. An explicit model argument takes precedence. Unknown providers and authentication/configuration errors are reported instead of triggering fallback.
