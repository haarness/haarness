# Haarness engineering docs

These pages explain how Haarness works and how to change it: behaviour in detail, server and
configuration reference, architecture, and dated engineering records. To learn to use Haarness,
start with the user guides at **<https://haarness.ai/docs>**; where a page here covers the same
ground, it links to the user guide at the top.

New to the code? Read the [README](../README.md) quick start, then [CONTRIBUTING.md](../CONTRIBUTING.md)
and the [build and run skill](../.skills/haarness-build-run/SKILL.md).

## Guides

- [Automatic approvals](AUTO-APPROVALS.md): Review and Auto, the per-chat approval mode the server keeps, and how Desktop and the TUI change it.
- [Browser alongside chat](BROWSER.md): Desktop's built-in browser and the user's Chrome, choosing between them, agent control, site permissions, local and remote networking.
- [Canvas and file previews](CANVAS-PREVIEW.md): interactive HTML and file previews in Desktop, saved versions, compare and restore.
- [Haarness Desktop](DESKTOP.md): first run, where things live, how each surface behaves, and Desktop development and packaging.
- [Desktop notifications](DESKTOP-NOTIFICATIONS.md): notifications that open the chat that finished or needs an answer.
- [Extensions](EXTENSIONS.md): TypeScript and JavaScript extensions that add tools and hooks, trust and installation.
- [Sign in to Haarness](HAARNESS-ACCOUNT.md): the Haarness account in the CLI, TUI and Desktop, Cloud models, billing commands, development sign-in.
- [Steer and Queue](INPUT-QUEUE.md): correcting work in progress versus queuing the next task, in both apps and the API.
- [Cross-project analytics](INSIGHTS.md): `haarness stats` and `haarness insights`, and how they count.
- [Reviewing large changesets](LARGE-DIFF-REVIEW.md): how Desktop and the API page through very large diffs.
- [Configuration profiles](PROFILES.md): save, inspect, restore and share portable configuration archives.
- [Remote computers through Haarness Cloud](REMOTE-ACCESS.md): reach your own machine from Cloud web, the API and the SDKs.
- [Haarness Router](ROUTER.md): connect a self-hosted router from Desktop, the TUI or the CLI.
- [Side questions with `/btw`](SIDE-QUESTIONS.md): ask about a chat without adding to it.
- [TUI message browser](TUI-MESSAGES.md): find, copy, reuse or fork at an earlier message (Alt+M, `/messages`).
- [API keys across clients](UNIFIED-API-KEYS.md): one `HAARNESS_API_KEY` for the CLI, Desktop and the SDKs.
- [Usage statistics](USAGE-STATISTICS.md): what the daily summary contains, when it is sent, and how to turn it off.
- [Dynamic workflows](WORKFLOWS.md): writing and running JavaScript/TypeScript workflows, their APIs and trust boundary.

## Reference

- [Server API v1](API.md): authentication, every resource group, events and capabilities.
- [Configuration and resource scopes](CONFIGURATION-SCOPES.md): where configuration, credentials and data live, precedence, instruction files, hooks.
- [Model catalogs](MODELS.md): live discovery, fallback catalogs, per-model metadata and update history.
- [Observability](OBSERVABILITY.md): logs and opt-in OpenTelemetry tracing, with the span and privacy contract.
- [Permissions and execution settings](PERMISSIONS-AND-EXECUTION.md): what an approval remembers, execution budgets, browser approvals.
- [Supported languages](../locales/README.md): the 17 interface catalogs and translation rules.
- [SDKs](../sdks/README.md): the generated TypeScript and Python SDKs and how they are tested.

## Architecture and development

- [Architecture](ARCHITECTURE.md): crate dependency rules, runtime invariants and the native boundary.
- [Projects, worktrees and chats](DESKTOP-PROJECT-MODEL.md): Desktop's project model, routing and remote hosts.
- [Haarness for Chrome](CHROME-EXTENSION.md): the Chrome extension, its native host bridge and protocols, setup and data layout, tests, packaging and Web Store publishing.
- [Desktop streaming Markdown](STREAMING-MARKDOWN.md): how replies render incrementally without reflowing.
- [Keeping local builds small](BUILD-STORAGE.md): Rust build settings and safe cache cleanup.
- [Desktop UX brief](DESKTOP-UX-ENGINEER-BRIEF.md): the brief given to UI/UX passes on Desktop.

## Engineering records

[Records](records/README.md) are dated verification reports, fix write-ups and model-support
audits. They describe the code at the time they were written and are not kept up to date; the
pages above are.
