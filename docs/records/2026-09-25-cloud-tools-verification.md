# Cloud management tools verification — 2026-09-25

Native base: `e99dd622cad2d2f4c7b1da5a89511d4ef39a1ed6`.
Cloud API/worker: `aa5c893cbd563fa3d0d817d4eca866b87bad3d5e`, running through the existing
Vagrant/Docker development stack at `http://localhost:18000`. Agent compute ran on Fly.io
in `nrt`, not on the Mac or in a local agent container. No production deployment was performed.

## Implementation boundary

`haarness-account::cloud::Cloud` owns account authentication, account scoping, control-plane
requests, attach tickets and remote runtime orchestration. Server tools supply schemas and
delegate through the CLI composition root. The core catalog and permission engine retain
control over discovery and execution. No new public server endpoints or SDK types are needed.

The shared login is checked for every invocation. Tokens and credential destinations are
host-owned, never model arguments. Template configuration is merged using its optimistic
revision; environment values stay in the Cloud editor/API. Planning can inspect but cannot
mutate or delegate remote work. Session reads do not start compute. Remote approvals are
reported as `needs_attention`; none are automatically approved by these tools.

## Automated checks

- `cargo test -p haarness-account`: **21 passed**. Real local HTTP fixtures cover signed-out
  rejection and logout, account isolation, pagination, template updates, model discovery,
  secret input rejection, ticket renewal after 401, asynchronous submission after startup,
  stopped-session reads, start transitions, history cursors, timeouts, permission prompts,
  cross-thread interrupt rejection, cancellation, archive/restore and delete routes.
- `cargo test -p haarness-core -p haarness-server -p haarness-cli`: **470 passed**. Includes
  plan ceilings, action/target permissions, tool registration/schema validation, lazy loading,
  native API, lifecycle, ACP and CLI regression suites.
- After the CLI routing correction, its **110 binary tests** passed again. Explicit Cloud
  providers retain upstream namespaced model IDs; existing direct and fully qualified forms
  are covered by the same test.
- `cargo clippy -p haarness-account --all-targets -- -D warnings`: passed.
- Focused server schema test passed again after final schema wording changes.
- CLI incremental build and final `cargo check -p haarness-cli`: passed.
- Existing opt-in Electron `cloud-sessions.spec.ts`: **1 passed**, using the real shared login
  and Fly session. It opened the session from the account dialog, saved/reopened its remote
  project, and verified the runtime's default agent.

## Live model, API and UI journey

The local orchestrator used `haarness/openai/gpt-5-mini` with lazy tools enabled. It discovered
both Cloud tools through `load_tool`, looked up the remote model, created a template, launched
an independent session and asked the remote `openai/gpt-5-nano` agent to write/read a file and
run `uname -s`. The result contained `CLOUD_TOOL_FLY_OK` and `Linux`.

- Template: `01a0d6b0-c23e-77f8-b525-168a581ed997` (Cloud tools E2E 20260925).
- Session: `01a0d6b0-c9a8-709c-9bb4-9a288abe650d` (Cloud tools verified).
- Conversation: `01a0d6b1-0258-7e72-a59f-2fad8d9630d1`.
- Workspace substrate: `01a0d6b0-c996-711f-a229-4644f7bd4e6f`.
- First Fly machine: `871543c0061168`; resumed generation: `8016e5c6610008`.

Chrome control opened the returned session URL and sent a follow-up that wrote
`browser-proof.txt` in `/persist/workspace`. The reply was `BROWSER_CLOUD_FOLLOWUP_OK`, and
the file appeared in the browser explorer. A real tmux TUI attached to the local orchestrator
then invoked `cloud_agent_session.messages` and read that browser reply from the same conversation.

Subsequent model calls retrieved the public SSH key, updated the template to revision 2,
and verified that the existing session retained revision 1. They renamed the session,
submitted an asynchronous sleep turn, interrupted the exact turn, and observed `interrupted`.
Stop completed through Cloud; a CLI model run then started generation 2 and read the same
persisted browser file in the original conversation. Chrome reconnected and displayed that
reply. Archive and restore preserved the session and its files.

The final CLI run used `--provider haarness --model openai/gpt-5-mini` and successfully called
archive with its default `true`, restored with `false`, and found `GPT-5 Nano` through the
human-readable search. No tool errors occurred in this final run. Earlier live calls caught
an excessive history limit (correctly rejected) and an omitted archive boolean; archive now
defaults to true and has dedicated coverage. The CLI run also exposed and fixed namespaced
Cloud IDs incorrectly switching to direct OpenAI authentication.

Separate live orchestrators using `anthropic/claude-haiku-4.5` and
`google/gemini-3.5-flash-lite` each loaded both schemas and successfully listed templates and
sessions. These are cross-provider tool-call checks, not a claim that every catalog model
or every permission/timeout scenario was exercised against production upstreams.

The test session is stopped after verification, preserving its example file for inspection.
Its Fly machine is removed by the existing stop lifecycle; the template and disk remain.
Only this test's local server and tmux session are shut down. Existing user sessions and the
Vagrant/Docker control plane are preserved.

## Repeat the main user flow

Use the same issuer/profile as the Cloud website, run `haarness login`, and ask in TUI,
Desktop or `haarness run`:

> Find my Repository helper cloud agent. Launch a session, ask it to write a test file in
> its workspace and read it back, and bring back the result and session link.

Open the link in Chrome and send a follow-up. Ask the original assistant to read the remote
messages, stop the session, resume it, and read the file again. Stop the session when finished.
See [account and Cloud usage](../HAARNESS-ACCOUNT.md) for CLI commands and lifecycle semantics.
