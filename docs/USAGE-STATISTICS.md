# Usage statistics

What the daily usage summary contains, when and how it is sent, and every way to turn it off. For developers changing the report, and for operators who need to keep CI or a fleet out of it.

> **User guide:** [Data and storage](https://haarness.ai/docs/security/data-and-storage).

Haarness shares a small daily summary of how it is used with Haarness Cloud, so the team can see which models, tools and features people rely on and where turns fail. It is on by default, and you can turn it off at any time.

## What is sent

One snapshot per UTC day for the last seven days, computed from the history already on your computer (the same history `haarness stats` reads):

- **Per day:** chats, turns (completed, failed, stopped), turns run by subagents and by schedules, input/output/cache tokens, known cost in USD, tool calls and tool errors, files changed and lines added/removed.
- **Per model:** provider and model id, turns, tokens and cost. Providers are named only when Haarness ships them (with their presets), and model ids only when Haarness's own model catalog lists them: a fine-tune or any other model is reported as `other`. Azure, Google Vertex and Amazon Bedrock model ids name your own deployments and inference profiles, so those models are reported as `custom`. A provider you configured yourself is reported as `custom` without its model; local runtimes (Ollama, LM Studio) report their models as `local`.
- **Per tool:** calls and errors for Haarness's built-in tools. Every MCP tool counts as `mcp`, every plugin or extension tool as `other`.
- **About the app:** the Haarness version, operating system, CPU architecture, release or development build, and a random installation id stored in `<data>/telemetry/state.json`.

**Never sent:** prompts, model answers, tool arguments or output, code, file names, paths, project or chat titles, custom provider, model or tool names, environment variables or credentials.

When you are signed in to Haarness Cloud, the report carries your sign-in token, so your statistics are linked to your account (and appear together across your computers). Otherwise the report is anonymous: only the installation id ties reports together.

See the exact report, at any moment:

```sh
haarness telemetry preview
```

Desktop shows the same report under **Settings → General → Share usage statistics → See what's shared**; the TUI under `/usage-statistics` → **Show what is shared**.

## When it is sent

A running Haarness server checks a minute after it starts and then every 30 minutes, and sends when the last report is more than six hours old. Each day's snapshot replaces the one Cloud already has for that day, so several servers, retries and restarts never count anything twice. `haarness telemetry send` sends now.

Reports come from the background server that the TUI, Desktop and `haarness run` start, so automation reports too. To keep CI and scripts out of the statistics, set `HAARNESS_TELEMETRY=0` there. Haarness's own test harnesses do: the SDK fixture server, tmux panes built with `scripts/e2e_tmux.pane_command`, the scripts that build their own environment, `scripts/test-all.sh` and the Desktop Playwright specs (except `usage-statistics.spec.ts`, which turns them on with Cloud on a closed local port), checked by `scripts/tests/test_e2e_isolation.py`.

## Turn it off

Any of these:

- Desktop: **Settings → General → Share usage statistics**.
- TUI: `/usage-statistics` (or `/telemetry`) → **Stop sharing usage statistics**.
- CLI: `haarness telemetry off` (`haarness telemetry on` turns it back on; `haarness telemetry status` shows the current state).
- Environment: `HAARNESS_TELEMETRY=0` (also `false`, `off`, `no` or `disabled`), or `DO_NOT_TRACK=1` (or `true`). The environment wins over the setting; `HAARNESS_TELEMETRY=1` (or `true`, `on`, `yes`, `enabled`) turns sharing on whatever the setting or `DO_NOT_TRACK` says. While the environment decides, Desktop and the TUI show the choice without letting you change it.

The setting is stored as `"telemetry": {"usage": false}` in the global `haarness.json`. Project configuration cannot change it: a repository you clone can neither turn sharing on nor off for you.

Cloud keeps usage statistics for 400 days; see the [privacy notice](https://haarness.ai/privacy). Development builds send to the local Cloud stack (`http://127.0.0.1:18080`), like every other Cloud call.

## Not to be confused with tracing

[OpenTelemetry tracing](OBSERVABILITY.md) is a separate, off-by-default export of detailed spans to a collector you choose. Turning usage statistics off does not affect it, and tracing never sends anything to Haarness.
