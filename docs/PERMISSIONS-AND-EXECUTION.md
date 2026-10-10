# Permissions and execution settings

Haarness uses one runtime policy for Desktop, attached TUI, CLI, workflows and nested tools. Ordinary workspace tools already run without approval. These settings reduce repeated prompts and let longer tasks finish while retaining explicit denials, cancellation and ordered writes.

## Choose what an approval remembers

- **Allow once** authorizes this request.
- **Always** in the TUI remembers it for every chat on the running server, until that server stops.
- **Always for this folder** persists it for the exact workspace.
- **Always for this repository** explicitly shares it with linked Git worktrees. In the TUI choose **Repository** (`r`), review the displayed rules, then press Enter.

In the TUI every choice that reaches past the one request (Always, This folder, Repository and Once + Auto) is staged first: the card names exactly what will be granted, and Enter, or the same key again once the card could be read, confirms. While the card has the keyboard only `y` (Allow once) and `n` (Reject) answer on one key. Any other key that is not an answer goes to the prompt, and letters the card took that turn out to start a message ("update…", "www…") move to the prompt without answering. Ctrl+G hands the keyboard to the prompt.

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

Shell output is streamed to a file with a bounded preview in memory. Large output exposes the full file path. Cancellation and deadlines terminate the shell process group on Unix; `0` removes the deadline, not the Stop control. Stdout stays separate in metadata for dynamic definition context. This does not add a new background-shell job API.

## Skills and plan mode

Dynamic `!` shell context in a skill or command now raises the ordinary permission prompt when it needs approval. A deny still prevents execution, and a nonzero context command still fails the invocation. Installed skill and enabled plugin helper directories receive read-only external-directory exemptions. Secret-file rules and canonical symlink checks still apply; these exemptions do not authorize writes or arbitrary home-directory access.

Plan mode permits browser tab inventory, frames, snapshots, find and screenshots. Browser evaluation, input, navigation and uploads retain their existing plan restriction. To allow an extension tool you have reviewed as observational, configure its exact name:

```json
{"execution":{"plan_read_tools":["ext__code_search__query"]}}
```

This is an operator assertion about that extension, not automatic analysis of JavaScript side effects. Explicit tool availability and permission denials still apply.

## Browser approvals

Settings → Browser offers **Remember site approvals**. It is off by default, preserving approval for every browser action on sites configured as **Requires approval**. Enable it to use ordinary session, folder or repository consent for each origin and capability (browsing, downloads, uploads). An existing site **Block** always remains effective. Every-time prompts offer Allow once and Deny in current clients.

Site rules accept exact origins and leading subdomain wildcards such as `https://*.example.com`. A wildcard includes nested subdomains, excludes the apex domain, and preserves scheme and port. Exact rules take priority, then the longest matching wildcard. `https://*.example.com` does not include `http://a.example.com`, another port, or `a.example.com.evil`.

An upload needing file access and site consent shows one combined approval. Every underlying gate is evaluated before the prompt; a deny in any gate prevents the upload. Remembered rules retain their original permission categories.

An unapproved redirect or script request to another origin still stops at the site boundary. Navigate to that origin explicitly to authorize it, or configure a matching site rule. Pausing arbitrary Chromium requests for an additional in-flight approval is not introduced in this change; it needs a separate lifecycle implementation that handles navigation deadlines, cancellation and uncertain submissions without replay.

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

Haarness Cloud actions that spend money or replace or delete something (launching, messaging, scheduling, changing or deleting Cloud sessions, templates and schedules) always ask, each time, with no **Always** choice; allow rules, `--yolo`, Auto approvals and hooks cannot skip the prompt, while a deny rule still refuses. See [Haarness account](HAARNESS-ACCOUNT.md).

Generated media retains its existing approval default. Users who want it to run automatically can set `permission.media_generate` to `"allow"` in their existing ordered permission configuration. Browser transfer and diagnostic limits retain their existing bounds; they are separate from inline media reads.
