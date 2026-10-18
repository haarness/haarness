# Permissions and execution settings

> **User guides:** [Permissions and approvals](https://haarness.ai/docs/security/permissions), [Execution budgets](https://haarness.ai/docs/security/execution-budgets), [Websites in your Chrome](https://haarness.ai/docs/security/permissions#websites-in-your-chrome). This page has the engineering detail.

Haarness uses one runtime policy for Desktop, attached TUI, CLI, workflows and nested tools. Ordinary workspace tools already run without approval. These settings reduce repeated prompts and let longer tasks finish while retaining explicit denials, cancellation and ordered writes.

## Choose what an approval remembers

- **Allow once** authorizes this request.
- **Always** in the TUI remembers it for every chat on the running server, until that server stops.
- **Always for this folder** persists it for the exact workspace.
- **Always for this repository** explicitly shares it with linked Git worktrees. In the TUI choose **Repository** (`r`), review the displayed rules, then press Enter.

A website in the user's Chrome is the exception: it offers only **Allow once** and **Allow in this chat**, which lasts until the server stops and never applies to another chat. See [Chrome approvals](#chrome-approvals).

In the TUI every choice that reaches past the one request (Always, This folder, Repository and Once + Auto) is staged first: the card names exactly what will be granted, and Enter, or the same key again once the card could be read, confirms. While the card has the keyboard only `y` (Allow once) and `n` (Reject) answer on one key. Any other key that is not an answer goes to the prompt, and letters the card took that turn out to start a message ("update…", "www…") move to the prompt without answering. Ctrl+G hands the keyboard to the prompt. On a Chrome card, `a` (Allow in this chat) answers on one key, because it reaches no further than this chat.

A command (or path, or address) longer than the card's rows is read in place before it is answered: ↑/↓ and PgUp/PgDn scroll it, Ctrl+E expands it to every row the card can spare and back, and the row under it says which lines are on screen. That row appears only while there is more to see; the answers never leave the screen, however short the terminal, and scrolling keeps a staged answer staged. A terminal under 40 columns or 12 rows has no room for the card, so it shows the request in one line, a row of its answers with their keys, and the approval hint in the status row instead of "working".

Repository consent uses the canonical Git common directory, never a remote URL or repository name. Independent clones and unrelated repositories retain separate approvals. Grants live in `repository-approvals.sqlite3` in Haarness's data directory. Existing folder approvals stay folder-scoped. A matching deny in the destination worktree remains effective. Non-Git folders offer folder/session consent only. Failed persistence keeps the prompt available to retry.

## Configure execution budgets

Use the global `~/.config/haarness/haarness.json{,c}` or a project's `.haarness/haarness.json{,c}`. Project fields override individual global fields. Existing TOML files accept the same keys in `[execution]`. Running work retains its configuration generation; subsequent work uses the reloaded configuration.

This example permits longer tools and two levels of task delegation:

```json
{
  "execution": {
    "shell_timeout_ms": 0,
    "subagent_concurrency": 6,
    "subagent_depth": 2,
    "read_parallelism": 8,
    "media_max_bytes": 20971520,
    "code_timeout_ms": 180000,
    "code_max_calls": 500,
    "code_max_source_bytes": 262144
  }
}
```

| Setting | Default | Meaning |
| --- | ---: | --- |
| `shell_timeout_ms` | 120000 | Default shell deadline in milliseconds; `0` disables it. A bash call can override it with `timeout`. The former 600000 ms maximum is removed. |
| `subagent_concurrency` | 4 | Running child agents per delegation depth. Must be positive. |
| `subagent_depth` | 1 | Root → child by default; `2` enables grandchildren; `0` disables task delegation. |
| `read_parallelism` | 8 | Consecutive native observations run together up to this count. Must be positive. |
| `media_max_bytes` | 20971520 | Inline media read limit, now 20 MiB. Must be positive. Client attachments and common provider adapters use a 20 MiB aggregate cap; Gemini retains its 10 MiB aggregate adapter budget. Provider-specific transport limits still apply. |
| `code_timeout_ms` | 60000 | Code Mode deadline; `0` disables it. |
| `code_max_calls` | 100 | Code Mode tool-call budget; `0` disables it. |
| `code_max_source_bytes` | 262144 | Code Mode source size; `0` disables this check. |
| `plan_read_tools` | `[]` | Exact trusted `ext__…` tool names allowed in plan mode. Wildcards are rejected. |

Reads, listings, globbing, grep, web fetches and task delegations can run together: a batch of `task` calls launches its child agents side by side, bounded by `execution.subagent_concurrency`, and the TUI lists each child under the batch row with its own live status. Writes, shell commands and tools without an explicit parallel-read contract remain ordering barriers. Batch calls preserve those barriers and return results in input order. Task agents have a separate pool at each depth so a parent waiting for a grandchild does not consume the grandchild's only slot. Cancellation still reaches descendants.

Child agents started together share a prompt prefix. On a provider with explicit prompt caching (Anthropic and Claude through the Haarness Cloud router), the first child's request goes alone and its siblings' first requests follow once its response begins streaming (at most 20 seconds later), so they read the cache it wrote instead of writing the same prefix again. See [Dynamic workflows](WORKFLOWS.md#context-api) for the details; the same applies to workflow agents.

A child agent (a task, a workflow and its agents) is bounded by every chat above it. Each one's rules and ceiling are checked on their own, so a parent whose rules end with a broad allow never reopens another's deny: the plan agent started from a build chat still cannot write (`PermissionEngine::add_inherited_ceiling`). These inherited denials refuse calls but never take a tool out of the child's list. Sessions of one agent then send one tool list whichever chat started them, so they share one cached prompt prefix, which starts with the tools. A call to a tool a parent denies outright fails with "tool image_generate is denied by the rule 'media_generate:*' of the chat or agent that started this one, which no rule here can lift. …". The child's own agent rules and ceiling (the plan agent's, a custom agent's `permission`) still take tools out of its list.

Shell output is streamed to a file with a bounded preview in memory. Large output exposes the full file path. Cancellation and deadlines terminate the shell process group on Unix; `0` removes the deadline, not the Stop control. Stdout stays separate in metadata for dynamic definition context. This does not add a new background-shell job API.

## Secret files

The built-in `read` rules (`PermissionEngine::build_defaults`, `crates/haarness-core/src/permissions/mod.rs`) ask before reading `*.env` and `*.env.*` files, such as `.env`, `prod.env` and `.env.local`. `*.env.example` stays readable, and so does a file inside a `node_modules` folder at any depth (`node_modules/bottleneck/.env`, `packages/app/node_modules/pkg/.env.test`), because packages ship `.env` files as fixtures. `mynode_modules`, `node_modules.bak` and `node_modules_x` are other folders. Configured rules come after these, so a configured `read` rule for `*.env` still covers a file inside `node_modules`. The rules match absolute paths too, so in a project whose own folder is inside a `node_modules` folder, a read that names a `.env` by its absolute path does not ask; configure `"*.env": "ask"` and `"*.env.*": "ask"` under `read` to keep the ask there.

- **Either spelling of a project file.** A `read`, `edit` (also `write`, `apply_patch` and `multiedit`) or `list` rule names a file inside the project whether the call gives its path relative to the project or absolute (`PermissionEngine::set_workspace`). `"read": {"secrets/*": "deny"}` refuses `read /path/to/project/secrets/key`, and `"edit": {"/path/to/project/vendor/*": "deny"}` refuses `edit vendor/lib.rs`, in configured rules, inherited ceilings and remembered answers alike. A path through a folder link, such as macOS's `/var` for `/private/var`, counts as inside the project too, and `./secrets/key`, `src/../secrets/key` and `build/` are `secrets/key`, `secrets/key` and `build`. A patch (`apply_patch`) meets the `edit` rules for every file it adds, changes, deletes or moves to (`*** Move to:`), and for where each really leads. The active skill's own rules follow the same matching. A rule starting with `*`, such as the built-in secret rules, matches the path as written, and a path outside the project keeps matching rules as written.
- **Where a file really is.** `read`, `edit`, `write`, `multiedit` and `list` also check the path the tool opens, after links and `..`, whenever it differs from the path the model gave (written workspace-relative when that was). A link `notes.txt -> .env` asks for `.env`, `node_modules/pkg/../../.env` asks like `.env`, and `list alias` for a link `alias -> build` meets `"list": {"build": "deny"}`. A rule that denies or asks for a link's target applies to the link. A workflow the agent creates (`workflow` with `action: create`) meets the `edit` rules for `.haarness/workflows/<name>.ts` and, when that folder is a link, for where the file really lands. An **Always** answer for such a read remembers both paths.
- **Hooks and extensions judge that file too.** A `PreToolUse` allow and a `permission.ask` or `PermissionRequest` answer about the path the model gave never settle an ask about the file it really opens. `PermissionRequest` hooks and `permission.ask` extensions are asked about that file in a call of its own, with the tool's `path` set to it (`tool_input.path` for hooks, `detail.path` for extensions) and the call's patterns naming it; the prompt is answered only when every call allows, and a deny in any refuses it. So a hook that allows reads unless the path ends in `.env` allows `notes.txt` but not the `.env` it leads to, and the person is asked. The same holds for a file path inside the project that a link takes outside it: the `external_directory` ask names the folder the file is really in, and hooks see that file's path. A direct tool call (a workflow's `ctx.tool()`, `!` shell) runs `PreToolUse` hooks but no `PermissionRequest` hooks, as before.
- **Search tools.** `grep`, `glob` and `list` never ask about secrets; they leave out every file these built-in rules ask about and end their output with the count, such as `(skipped 2 secret-named files)`, also given as `skipped` in the result metadata. A file is checked by its own path, by the path the model named (a search path may run through a link), and, for a link, by where it leads, so a linked workspace package under `node_modules` does not hand out its `.env`. `grep` checks a file or link given as its path before reading it, and returns `No files found` with the note. Its tree walk skips hidden files and folders (including a `.env` virtualenv) and does not follow links, so those are neither searched nor counted. When `glob` stops at its 10,000-result limit, or `grep` at its 20,000-file limit, the note says `(skipped at least N secret-named files)`. Folders stay listed; only files are left out.
- **Files a read rule denies.** `grep` and `glob` also leave out every file a `read` rule denies for the agent searching (configured, the agent's own `permission`, or a chat above it), judged the same three ways, and count them apart: `(skipped 2 files a read rule denies)`, or `(skipped 1 secret-named file and 2 files a read rule denies)`, with `denied` in the metadata beside `skipped`. So `"read": {"private/*": "deny"}` keeps `grep` from returning a line of `private/key.txt` and `glob` from naming it. `list` still names such files (it never reads them), an `ask` rule hides nothing, and a workflow script's direct `ctx.tool("grep", …)` call leaves out secret names only.
- **Instruction imports.** An `@import` in `AGENTS.md` or another instruction file never reads a secret-named file or a file a configured `read` rule denies, judged by where it really leads; `[Instruction import omitted: <path> (…)]` takes its place in the prompt. See [configuration scopes](CONFIGURATION-SCOPES.md).
- **Not covered.** An explicit `read` of a secret-named file still asks, and Auto answers it like any other eligible request. A `bash` command such as `cat .env` is not checked against the `read` rules. The file search behind `@` completion and Desktop's explorer and search (`GET /v1/files/search`) serves people, not the model, and still lists such files; a file a person picks there is attached by their own choice.

## Shell commands outside the project

`bash` runs without asking under the default rules, but a command that reaches outside the project asks `external_directory` for the folder it reaches (`crates/haarness-core/src/permissions/bash.rs`). The check reads the arguments of `cd`, `rm`, `cp`, `mv`, `mkdir`, `touch`, `chmod`, `chown` and `cat`, and every redirect target, resolved as the shell would from the project folder:

- **New paths.** A path that does not exist yet is judged where it would be created, so `touch /tmp/new`, `mkdir -p /tmp/a/b`, `cp notes.txt /tmp/new` and `echo x > /tmp/new` ask like an existing file there. `..` inside the part that does not exist is taken the way `mkdir -p` creates it.
- **Links.** Links are followed, dangling ones too (writing through one creates its target), and `..` after a link goes up from where the link leads. A link chain the check cannot finish counts as outside (`/*`).
- **Home.** An unquoted leading `~` is the home folder, and `~name` that user's home (taken next to yours); quoted, `~` is just a name in the project. A leading `$HOME` or `${HOME}`, bare or in double quotes, is the home folder too, and so are `$TMPDIR`, `$HAARNESS_HOME`, `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME` and `$XDG_STATE_HOME` when the server has them set to an absolute folder: `cp x.json $HOME/.config/haarness/haarness.json`, `echo x > "${HOME}/.profile"` and `cd $HOME` ask like their `~` spellings. In single quotes they are names.
- **Dynamic parts.** A value such as `/tmp/$name` or `~/$(date)` is judged by the folder its literal start names; one that starts with another variable, an expansion with an operator (`${HOME:-/x}`) or a substitution, such as `$OUT/x`, is not checked.
- **Not covered.** Other programs' arguments are not read (`tee /tmp/x`, `sed -i … /etc/x`, `python -c …`), nor paths relative to a `cd` earlier in the command. This guards against slips; it is not a sandbox.

## Haarness's own configuration files

An agent that rewrites the files Haarness takes permissions, hooks, MCP servers and providers from could widen its own permissions at the next reload: a project whose rules ask before `bash` would stop asking once the agent added `"bash": "allow"` to its `haarness.json`. So `edit`, `write`, `multiedit` and `apply_patch` ask a person before changing `haarness.json` or `haarness.jsonc`, `opencode.json` or `opencode.jsonc`, `.haarness/config.toml` (and `.haarness/cloud/haarness.json`), `.haarness/extensions.json` and any file under `.haarness/extensions/` (extension code, which can answer approvals), `.claude/settings.json`, `.claude/settings.local.json`, `.mcp.json`, a plugin's `hooks/hooks.json` and `.claude-plugin/plugin.json` (a project's own plugins under `.haarness/plugins/` load as trusted), and the global configuration folder's `config.toml`, `extensions.json` and `extensions/` (`$HAARNESS_HOME/config`, else `$XDG_CONFIG_HOME/haarness`, else `~/.config/haarness`). They are matched at any depth, case-insensitively, without the trailing dots and spaces Windows drops (so `haarness.json ` and `.mcp.json.` count), and after `.`, `..` and links (a link `notes.json -> haarness.json` asks too) (`is_config_path` in `crates/haarness-core/src/permissions/mod.rs`). Reading them is unchanged.

An agent, command or skill definition can widen what runs, too: an agent's `permission`, `permissions` or `tools`, a command's or skill's `allowed-tools` (granted while it runs) and Claude's `permissionMode`. An edit of a markdown file under an `agents`, `agent`, `commands` or `command` folder, or of a `SKILL.md`, asks the same way when it adds, changes or removes one of those fields (removing `permission: {bash: deny}` widens an agent as much as adding an allow); an edit of the prompt, the description or any other field runs as before (`definition_grant_edits` in `crates/haarness-core/src/runtime.rs`).

- **Only a person answers.** The request offers **Allow once** and **Reject**, never **Always**, and nothing answers it in a person's place: not Auto, not a turn that approves its own requests (`haarness run --yolo`, a `--yolo` terminal UI, ACP), not `serve --yolo`, not a `PreToolUse` or `PermissionRequest` hook. A scheduled run, which cannot ask, refuses it at once ("'edit:haarness.json' needs the user's approval every time, and this scheduled run cannot ask …") and goes on, as it does for Cloud actions. `haarness run` without a terminal stops on it.
- **Rules that open it.** An allow rule whose last path segment holds the file's name, such as `"edit": {"haarness.json": "allow"}`, `"**/haarness.json": "allow"` or `"*.mcp.json": "allow"`, lets the edit run without asking, even when a blanket allow comes after it. Other allows never do: `"edit": "allow"`, `"*": "allow"`, `"**/*"`, a folder (`".claude/**"`, `".haarness/**"`), a kind of file (`"*.json"`) and an **Always** answer to another edit (which remembers `edit:*`) leave it asking. A deny refuses it as usual, and the plan agent still cannot write it.
- **Not covered.** A `bash` command that writes one of these files (`echo … > haarness.json`, `sed -i`) is not checked: the shell analysis cannot see every write, so this guards the file tools, not the shell. A command outside the project, such as one that writes `~/.config/haarness/haarness.json`, still asks for that folder (see [Shell commands outside the project](#shell-commands-outside-the-project)). The management tools (`mcp_manage`, `agent_manage` and the others) change configuration under their own permissions. A plugin's `mcpServers` may also name other JSON files inside the plugin; only `plugin.json` and `.mcp.json` are covered. A `patch` that changes a definition file is judged by applying its hunks to the file as it is now.

## Web fetches

`webfetch` rules (`webfetch:<address>`) meet the address as written and, when it is written differently, the address the fetch really reads: scheme and host in lower case, without a user name or password, a trailing dot on the host, the default port or `.`/`..` path segments. So a rule for `https://example.com/*` also applies to `HTTPS://user@Example.com.:443/a/../b`; another port is another address.

A fetch follows redirects only while they stay on the host it asked for (the same host and port, or the same host moving from http to https). Any other redirect (to another host, another port, or from https to plain http) is not followed: the result names the new address and why, and fetching it is the model's next call, checked against the rules like any other. Image and video generation follow their configured backend's redirects as before. A redirect within the host is followed without a new check, so a rule for one path of a host does not stop a redirect to it from another path of the same host.

## Skills and plan mode

Dynamic `!` shell context in a skill or command now raises the ordinary permission prompt when it needs approval. A deny still prevents execution, and a nonzero context command still fails the invocation. Installed skill and enabled plugin helper directories receive read-only external-directory exemptions. Secret-file rules and canonical symlink checks still apply; these exemptions do not authorize writes or arbitrary home-directory access.

Plan mode permits browser tab inventory, frames, snapshots, find and screenshots. Browser evaluation, input, navigation and uploads retain their existing plan restriction. To allow an extension tool you have reviewed as observational, configure its exact name:

```json
{"execution":{"plan_read_tools":["ext__code_search__query"]}}
```

This is an operator assertion about that extension, not automatic analysis of JavaScript side effects. Explicit tool availability and permission denials still apply.

## Browser approvals

This section is about Desktop's Haarness browser; Chrome follows [Chrome approvals](#chrome-approvals). Settings → Browser offers **Remember site approvals**. It is off by default, preserving approval for every browser action on sites configured as **Requires approval**. Enable it to use ordinary session, folder or repository consent for each origin and capability (browsing, downloads, uploads). An existing site **Block** always remains effective. Every-time prompts offer Allow once and Deny in current clients.

Site rules accept exact origins and leading subdomain wildcards such as `https://*.example.com`. A wildcard includes nested subdomains, excludes the apex domain, and preserves scheme and port. Exact rules take priority, then the longest matching wildcard. `https://*.example.com` does not include `http://a.example.com`, another port, or `a.example.com.evil`. A host's trailing dot (`https://example.com./`, the fully qualified spelling) is the same site in rules and requests, so it neither escapes a Block nor needs a rule of its own; a rule saved with one is stored without it.

An upload needing file access and site consent shows one combined approval. Every underlying gate is evaluated before the prompt; a deny in any gate prevents the upload. Remembered rules retain their original permission categories.

An unapproved redirect or script request to another origin still stops at the site boundary. Navigate to that origin explicitly to authorize it, or configure a matching site rule. Pausing arbitrary Chromium requests for an additional in-flight approval is not introduced in this change; it needs a separate lifecycle implementation that handles navigation deadlines, cancellation and uncertain submissions without replay.

## Chrome approvals

An agent in the user's Chrome acts in the user's signed-in accounts, so a site that **Chrome site permissions** sets to **Requires approval** (the default for browsing, downloads, uploads and scripts) needs a person's answer, per chat and per origin. Only a person's answer counts, given in a client of this server other than a published chat; nothing configured in advance can give it, except a site the user sets to **Always allow** in the global Chrome settings, which no project can loosen (see [Chrome settings](BROWSER.md#chrome-settings)).

**Patterns.** For each HTTP(S) origin a Chrome command touches, the gate pushes the remember pattern `chrome-<category>:<thread id>:<origin>` (checked as the operation `browser_site:chrome-…`), with categories `browsing`, `downloads` and `uploads`, plus `scripts` for `browser_evaluate` even when browsing is already approved. Navigation and new tabs include the origin's `www.` twin. Because the pattern carries the thread id, a remembered answer never covers another chat. `BrowserTool::explicit_approval` returns true for operations starting `browser_site:chrome-`, and `RedefinedTool` in `crates/haarness-extensions` forwards it so a runtime extension that redefines a browser tool cannot drop it.

**What satisfies them.** `PendingPermission.explicit` is true when any asked pattern is explicit. An explicit pattern is satisfied only by an answered grant in the runtime's `session_rules` whose rule starts with `browser_site:chrome-` (`explicit_granted`). Configured rules (defaults, global and project configuration, Claude-compatible JSON, custom agents, `"*"`, `browser_site:*`) can only deny it.

**Bypasses that do not apply.** Explicit requests are treated like forced ones at every bypass: the direct-path and model-path hook skips, direct-path and model-path Auto, the release of waiting prompts when a chat switches to Auto, server `--yolo` (which also stores no session rule for them), a `PermissionRequest` hook's Allow, and the refresh fast path, which checks `explicit_granted`. Unlike forced requests they offer an "always" answer (`allowAlways: true`), store it, and may be released when an answered rule covers them.

**Answers.**

| Reply | Effect |
| --- | --- |
| `once` | This call |
| `always` | A session rule for this chat, origin and category, until the server stops |
| `always_workspace`, `always_repository` | Downgraded to `always`; never persisted |
| `reject` | The call fails |

Desktop's dock and the TUI card read `explicit` and show **Allow in Chrome** with humanised lines ("Use https://example.com in Chrome", "Send downloads from … in Chrome to the agent", "Upload files to … in Chrome", "Run scripts on … in Chrome"). Desktop offers **Deny**, **Allow once** and **Allow in this chat** (primary, `always`); the TUI uses `a` (Allow in this chat, a single press), `y` (once) and `n`, with no `w`, `r` or `u`. Both show the call's own arguments under those lines (the address, script, text or files), without `__browser`, so Allow once approves a call the person has seen. The popup of the Haarness for Chrome extension can answer the same request through the bridge, which forwards only explicit approvals waiting on a Chrome site, with only their Chrome patterns. One that also waits on another gate (for example a file outside the project for an upload) shows in the popup with "Answer it in Haarness", and the bridge refuses to answer it.

**Who cannot answer.**

- Client auto-repliers skip explicit requests: the TUI's `--yolo` and client-side Auto replies, `haarness run --yolo` and `workflow run --yolo` watchers, ACP, and Desktop's fallback auto-approver for older servers.
- `haarness run` at a terminal shows the call's arguments (the URL, script, text or files) and asks `[y] allow in this chat / [o] once / [N]`. Without a terminal it stops with "Stopped: the agent asked to use &lt;origin&gt; in Chrome, which always needs a person. Answer in the TUI or Desktop, or allow the site under Settings → Browser → Chrome."
- Scheduled runs, and the subagents, workflows and workflow agents they start, fail immediately, in turns and in a workflow's own tool calls: "Chrome needs the user's approval for &lt;origin&gt; and this run cannot ask. Ask the user to allow it in an interactive chat, then retry." Only the run counts as unattended: the first turn a schedule starts in the chat it created, and the work that turn starts (a workflow keeps it after the turn ends). A person who later types in that chat, runs `!` there or works in a fork of it is asked as usual. `--yolo` clients (the TUI, `haarness run`, ACP) and their workflow agents still ask.
- Requests scoped to a published chat (one shared with `/remote-control`, carrying `x-haarness-remote-thread`) answering an explicit prompt get 403 "Answer this on the computer running Chrome." A computer connected whole with `haarness remote enable` (Remote control › Hosts) relays unscoped requests with the local token, so an answer from there counts like one from a local client.

**API.** `PendingPermissionInfo.explicit` (`#[serde(default)]`) and the `permission.requested` event payload's `explicit: true` let clients tell these requests apart; both are in the OpenAPI document and the generated SDKs. Haarness browser `browser_site:` gates without the `chrome-` prefix behave as before.

## Extension budgets

In global or project `extensions.json`, keep `timeout_ms` as the fallback and override the stages that need more time:

```json
{
  "enabled": true,
  "timeout_ms": 30000,
  "init_timeout_ms": 60000,
  "hook_timeout_ms": 10000,
  "tool_timeout_ms": 0,
  "payload_max_bytes": 8388608,
  "entries": []
}
```

A deadline of `0` disables that deadline. Missing stage settings inherit `timeout_ms`; omitted project fields inherit global settings. The former five-minute maximum is removed. Message size defaults to 2 MiB and remains a positive configurable bound. Cancellation still terminates the host; timed-out or cancelled tools are not replayed. Large artifacts should normally be returned as files.

Haarness Cloud actions that spend money or replace or delete something (launching, messaging, scheduling, changing or deleting Cloud sessions, templates and schedules) always ask, each time, with no **Always** choice; allow rules, `--yolo`, Auto approvals and hooks cannot skip the prompt, while a deny rule still refuses. A scheduled run, and the agents and workflows it starts, refuses such a request at once instead of waiting for a person: "'cloud_agent_manage:delete:…' needs the user's approval every time, and this scheduled run cannot ask. Do not retry it here; ask the user to do it in an interactive chat." The turn goes on with that refusal. See [Haarness account](HAARNESS-ACCOUNT.md). Chrome site approvals cannot be skipped that way either, but can be remembered for one chat; see [Chrome approvals](#chrome-approvals).

Generated media retains its existing approval default. Users who want it to run automatically can set `permission.media_generate` to `"allow"` in their existing ordered permission configuration. Browser transfer and diagnostic limits retain their existing bounds; they are separate from inline media reads.
