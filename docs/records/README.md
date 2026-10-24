# Engineering records

Dated write-ups of what was changed, verified or audited, newest first. Each one describes the
code at its date and is not updated afterwards: for current behaviour, read the
[engineering docs](../README.md) or the user guides at <https://haarness.ai/docs>.

Name new records `YYYY-MM-DD-topic.md` with the date the work was verified, and add a line here.

- [2026-10-24 Chats without a project](2026-10-24-no-folder-chats.md): Desktop opens ready to chat: This computer lists chats that each work in a folder of their own inside one chat home, the message box's folder chip picks No folder or a project, and the file APIs, tools and permissions keep each chat inside its folder; also the transcript that bounced when a card left and a crash dialog on quit.
- [2026-10-24 UX polish](2026-10-24-ux-polish.md): The TUI and Desktop used as a person would and smoothed: approval keys that never answer a typed reply, drafts and the keyboard kept through sends and switches, steps that stay where they were clicked, preview chat tabs, a sidebar and Search that keep their place, Settings that list what is connected, and translations brought up to date.
- [2026-10-15 Model support audit](2026-10-15-model-support.md): Gemini 4 Argon, with probe results and live runs.
- [2026-10-10 Model support audit](2026-10-10-model-support.md): Claude 5.5, GPT-6.1 Sol and other new models, with probe results.
- [2026-09-28 Router profiles verification](2026-09-28-router-profiles-verification.md): multiple named Cloud routers.
- [2026-09-28 Release and SDK verification](2026-09-28-release-sdk-verification.md): binary releases, installers and the SDKs.
- [2026-09-27 Unified API keys verification](2026-09-27-unified-api-keys-verification.md): one API key across the CLI, Desktop and the SDKs.
- [2026-09-27 Cloud SDK verification](2026-09-27-cloud-sdk-verification.md): Cloud sessions, chats, schedules and their SDK handles.
- [2026-09-27 Reactive Cloud tools verification](2026-09-27-cloud-tools-reactive-verification.md): push-based waits in the Cloud agent tools.
- [2026-09-26 Reliability verification](2026-09-26-reliability-verification.md): agent switching, filtered responses, Cloud model discovery and more.
- [2026-09-25 Cloud tools verification](2026-09-25-cloud-tools-verification.md): the native Cloud management tools against a dev stack.
- [2026-09-23 Desktop startup crash](2026-09-23-desktop-startup-crash.md): an Electron crash when a window opened while quitting.
- [2026-09-23 Model support audit](2026-09-23-model-support.md): provider catalogs and API rules, with [per-model results](2026-09-23-model-support.json).
- [2026-09-13 Task continuations](2026-09-13-task-continuations.md): parallel calls continuing one delegated child.
- [2026-09-12 TUI parity](2026-09-12-tui-parity.md): Desktop/TUI parity gaps closed, and how they were checked.
- [2026-09-12 Reliability fixes](2026-09-12-reliability-fixes.md): redirect parsing, MCP recovery, approvals, LSP timeouts and later fixes to September 23.
- [2026-09-07 Desktop verification](2026-09-07-desktop-verification.md): Desktop scenarios and feature checks, September 7–23.
