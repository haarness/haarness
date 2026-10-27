# Haarness engineering docs

These pages explain how Haarness works and how to change it. **To learn to use Haarness, start with
the user guides at <https://haarness.ai/docs>**; where a page here covers the same ground, it links
to the guide at the top and adds the engineering detail.

New to the code? Read the [README](../README.md) quick start and the [reference](REFERENCE.md), then
[CONTRIBUTING.md](../CONTRIBUTING.md) and the [build and run skill](../.skills/haarness-build-run/SKILL.md).

## Using Haarness

- [Automatic approvals](AUTO-APPROVALS.md): Review, Auto and Yolo, the per-chat approval mode the server keeps, and Auto's reviewer model.
- [Browser alongside chat](BROWSER.md): Desktop's built-in browser and the user's Chrome, choosing between them, agent control, site permissions, local and remote networking.
- [Canvas and file previews](CANVAS-PREVIEW.md): interactive HTML and file previews in Desktop, saved versions, compare and restore.
- [Haarness Desktop](DESKTOP.md): first run, where things live, how each surface behaves, and Desktop development and packaging.
- [Desktop notifications](DESKTOP-NOTIFICATIONS.md): notifications that open the chat that finished or needs an answer.
- [Extensions](EXTENSIONS.md): TypeScript and JavaScript extensions that add tools and hooks, trust and installation.
- [Sign in to Haarness](HAARNESS-ACCOUNT.md): the Haarness account in the CLI, TUI and Desktop, Cloud models, agents and workspaces, billing commands, development sign-in.
- [Steer and Queue](INPUT-QUEUE.md): correcting work in progress versus queuing the next task, in both apps and the API.
- [Cross-project analytics](INSIGHTS.md): `haarness stats` and `haarness insights`, and how they count.
- [Reviewing large changesets](LARGE-DIFF-REVIEW.md): how Desktop and the API page through very large diffs.
- [Marketplace and capabilities](MARKETPLACE.md): `haarness marketplace` and `haarness capability`, verified installs, receipts, and turning installed resources on and off.
- [Configuration profiles](PROFILES.md): save, inspect, restore and share portable configuration archives.
- [Prompt files](PROMPT-FILES.md): replace or add to the built-in prompts from the global or system folder, and `haarness prompt`.
- [Remote computers through Haarness Cloud](REMOTE-ACCESS.md): reach your own machine from Cloud web, the API and the SDKs.
- [Haarness Router](ROUTER.md): connect a self-hosted router from Desktop, the TUI or the CLI.
- [Schedules](SCHEDULES.md): `haarness schedule`, how schedules are registered with launchd, systemd and Task Scheduler, and what an unattended run does.
- [Side questions with `/btw`](SIDE-QUESTIONS.md): ask about a chat without adding to it.
- [TUI message browser](TUI-MESSAGES.md): find, copy, reuse or fork at an earlier message (Alt+M, `/messages`), and move between messages (Alt+Up/Down).
- [API keys across clients](UNIFIED-API-KEYS.md): one `HAARNESS_API_KEY` for the CLI, Desktop and the SDKs.
- [Usage statistics](USAGE-STATISTICS.md): what the daily summary contains, when it is sent, and how to turn it off.
- [Dynamic workflows](WORKFLOWS.md): writing and running JavaScript/TypeScript workflows, their APIs and trust boundary.

## Reference

- [Haarness reference](REFERENCE.md): the long-form tour — command examples, servers and clients, worktrees, the terminal UI, agents and workflows, schedules, plugins, skills and MCP, providers and the full configuration.
- [Server API v1](API.md): authentication, every resource group, events and capabilities.
- [Agent Client Protocol](ACP.md): `haarness acp` for editors — the messages it handles, sessions, prompts, MCP and approvals, and its tests.
- [Configuration and resource scopes](CONFIGURATION-SCOPES.md): where configuration, credentials and data live, precedence, instruction files, lifecycle hooks.
- [Model catalogs](MODELS.md): live discovery, fallback catalogs, per-model metadata and update history.
- [Observability](OBSERVABILITY.md): logs, `haarness logs`, and opt-in OpenTelemetry tracing, with the span and privacy contract.
- [Permissions and execution settings](PERMISSIONS-AND-EXECUTION.md): what an approval remembers, execution budgets, secret files, MCP resources, browser and Chrome approvals.
- [Supported languages](../locales/README.md): the 17 interface catalogs and translation rules.
- [SDKs](../sdks/README.md): the generated [TypeScript](../sdks/typescript/README.md) and [Python](../sdks/python/README.md) SDKs and how they are tested.
- [Bundled ripgrep](../vendor/ripgrep/README.md): the embedded `rg` and how it is verified.

## Architecture and development

- [Architecture](ARCHITECTURE.md): crate dependency rules, runtime invariants and the native boundary.
- [Terminal UI internals](TUI.md): the TUI's crates and modules, the files it keeps, and how it is tested headless and in tmux.
- [Projects, worktrees and chats](DESKTOP-PROJECT-MODEL.md): Desktop's project model, routing and remote hosts.
- [Haarness for Chrome](CHROME-EXTENSION.md): the Chrome extension, its native host bridge and protocols, setup and data layout, tests, packaging and Web Store publishing. Working on it: [apps/browser-extension](../apps/browser-extension/README.md) and the shared [browser driver](../packages/browser-driver/README.md).
- [Desktop streaming Markdown](STREAMING-MARKDOWN.md): how replies render incrementally without reflowing.
- [Keeping local builds small](BUILD-STORAGE.md): Rust build settings and safe cache cleanup.
- [Desktop UX brief](DESKTOP-UX-ENGINEER-BRIEF.md): the brief given to UI/UX passes on Desktop.
- [Brand assets](../brand/README.md): the logo, wordmark and app icons, and how they are generated.
- Skills for development work: [build and run](../.skills/haarness-build-run/SKILL.md),
  [releases](../skills/haarness-release/SKILL.md), and the testing skills in [`skills/`](../skills/)
  (Cloud workspaces and schedules, remote access, the marketplace, SDKs, billing, storage, sign-in, email).

## Records

[Engineering records](records/README.md) are dated verification reports, fix write-ups and
model-support audits. They describe the code at the time they were written and are not kept up to
date; the pages above are.

## Find a feature

| Feature | Engineering docs |
| --- | --- |
| Install, upgrade, release | [Install options](REFERENCE.md#install-options), [Desktop packaging](DESKTOP.md#package-and-launch), [release skill](../skills/haarness-release/SKILL.md) |
| Servers, `serve`, `attach`, `run` | [Servers and clients](REFERENCE.md#servers-and-clients), [API](API.md), [Architecture](ARCHITECTURE.md) |
| Terminal UI | [Terminal UI](REFERENCE.md#terminal-ui), [TUI internals](TUI.md), [message browser](TUI-MESSAGES.md), [`/btw`](SIDE-QUESTIONS.md), [Steer and Queue](INPUT-QUEUE.md) |
| Desktop | [Desktop](DESKTOP.md), [projects](DESKTOP-PROJECT-MODEL.md), [previews](CANVAS-PREVIEW.md), [browser](BROWSER.md), [notifications](DESKTOP-NOTIFICATIONS.md), [large diffs](LARGE-DIFF-REVIEW.md) |
| Editors | [ACP](ACP.md) |
| Providers, sign-in, models | [Providers](REFERENCE.md#providers), [model catalogs](MODELS.md), [Haarness account](HAARNESS-ACCOUNT.md), [API keys](UNIFIED-API-KEYS.md), [Router](ROUTER.md) |
| Approvals and permissions | [Automatic approvals](AUTO-APPROVALS.md), [permissions](PERMISSIONS-AND-EXECUTION.md) |
| Configuration, instructions, hooks | [Configuration](REFERENCE.md#configuration), [scopes](CONFIGURATION-SCOPES.md), [lifecycle hooks](CONFIGURATION-SCOPES.md#lifecycle-hooks), [prompt files](PROMPT-FILES.md), [profiles](PROFILES.md) |
| MCP servers | [MCP in the reference](REFERENCE.md#plugins-skills-and-tools), [MCP resources](PERMISSIONS-AND-EXECUTION.md#mcp-resources), [MCP App pages](DESKTOP.md#mcp-app-pages), [API](API.md) |
| Skills, agents, commands, plugins | [Plugins, skills and tools](REFERENCE.md#plugins-skills-and-tools), [scopes](CONFIGURATION-SCOPES.md), [marketplace](MARKETPLACE.md) |
| Extensions | [Extensions](EXTENSIONS.md) |
| Subagents, goals, workflows | [Agents, goals and workflows](REFERENCE.md#agents-goals-and-workflows), [workflows](WORKFLOWS.md) |
| Schedules | [Schedules](SCHEDULES.md) |
| Worktrees and chats | [Worktrees](REFERENCE.md#worktrees), [projects](DESKTOP-PROJECT-MODEL.md), [command examples](REFERENCE.md#command-examples) |
| Your Chrome | [Haarness for Chrome](CHROME-EXTENSION.md), [browser](BROWSER.md) |
| Haarness Cloud and remote access | [Haarness account](HAARNESS-ACCOUNT.md), [remote computers](REMOTE-ACCESS.md) |
| Statistics and logs | [Insights](INSIGHTS.md), [usage statistics](USAGE-STATISTICS.md), [observability](OBSERVABILITY.md) |
| SDKs | [SDKs](../sdks/README.md), [API](API.md) |
