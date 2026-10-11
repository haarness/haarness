# Interactive parity and verification

Updated September 12, 2026. These changes cover the actionable Desktop/TUI gaps identified in client workflows. Configuration, credentials and execution stay on the selected Haarness server.

## What is available

| Workflow | Haarness behavior |
| --- | --- |
| Servers and projects | Desktop has separate settings panes and a server switcher. TUI `/servers` saves connections, opens server folders and selects projects. Switching replaces REST, event-stream and terminal connections together; old server work continues and drafts remain scoped to their endpoint. |
| Models and reasoning | Desktop combines model and reasoning selection. Both clients use the selected model's advertised effort levels and clear unsupported choices. Provider artwork appears beside provider names; model rows do not repeat it. |
| Agent selection | Hidden and subagent-only profiles are excluded from primary selection and cycling. Desktop composer colors follow the active agent. Subagents remain available through explicit agent references and delegation. |
| Providers | `/providers` manages API keys, supported device/browser logins, custom endpoints, and source-aware editing/removal. Credentials are masked and remain in the native credential store. Stale custom-provider edits are rejected. |
| MCP | `/mcp` adds local/remote servers, edits connections, enables/disables/restarts them, and manages OAuth. A remote client can paste the browser callback URL. Authorization uses PKCE; cancellation stops the job and completed jobs report their actual result. |
| Schedules | `/schedules` creates disabled schedules, edits interval/cron/one-time triggers, enables/disables/removes schedules, runs them immediately, and shows durable run history. Immediate runs use the native scheduler runner. |
| Hooks, memory, profiles | `/hooks`, `/memory` and `/profiles` use existing services. Memory and hook edits retain revision checks. Profile restore shows affected files and requires an explicit replacement action. |
| Rich prompt context | `@file:start-end` attaches a bounded server-read snapshot. `@agent` attaches an explicit delegation instruction. References survive stash/history/editor round trips; deleting a reference removes it as a unit. |
| Prompt preferences | `Ctrl+F` in the model picker favorites a model; recents influence ordering. `/stash`, `Ctrl+S` and `Alt+S` save/restore unsent prompts. `/keybinds` edits supported shortcuts and rejects conflicts. |
| Context and usage | `/usage` or `/context` shows measured tokens, cache usage, cost availability, model context size and delegated descendants. Unavailable measurements are identified. Desktop provides its conversation context inspector. |
| Sharing | `/share` and Desktop Session actions → Share create a private preview, publish that exact text after confirmation, and return a revocable link. Links expire after seven days. Tool output is opt-in. HTML is escaped and served with restrictive headers. |
| Remote settings | Desktop's remote provider, MCP, hook, schedule, profile and memory forms call the selected server's management API. Local rich editors continue using their established services. |

## Architecture

`haarness-protocol` defines `ManagementView`, fields and actions. The authenticated `POST /v1/manage/{resource}` route delegates to the CLI composition layer's management service. That adapter calls the existing provider credential store, MCP registry, scheduler, hook/memory services and profile catalog. Both TUI and Desktop render the same server-authored forms; neither edits a remote host's files locally. Form values survive failed submissions. Password fields are masked, and Desktop ignores responses from an obsolete connection/view.

Model capabilities and selectable-agent rules remain protocol/domain decisions. UI components do not invent a universal reasoning scale. Provider artwork is bundled and resolved through the existing provider identity registry.

Tool execution uses ordinary host permissions, with application-level allow/ask/deny checks enforced by the server for every client. There are no tool sandbox profiles, hostname proxies, Seatbelt or bubblewrap wrappers. Retired `sandbox.json` files are ignored. Sharing stores only the selected preview text, not a restorable session archive. Preview storage is private and separate from the public route. Access tokens are omitted from HTTP trace paths.

## Platform and service boundaries

- Shared links are hosted by the user's Haarness server. Readers need network access to that server. No external hosting account is required.
- Sharing publishes the text shown in the preview. Review it for secrets; ordinary message text is not automatically redacted. The snapshot freezes at preview time, so later messages cannot slip into publication.
- OAuth was tested with an actual local PKCE authorization/token fixture through the server management API, including pasted remote callbacks and cancellation. No real subscription account was authenticated during testing.
- Saved server credentials and local draft/stash files use private file permissions. A normal fresh launch still opens a new conversation; saved drafts restore when their conversation/server is revisited.

## Historical verification (before tool sandbox removal)

Passed functional gates:

- CLI/server/TUI/MCP Rust suites: 602 tests passed, including 308 TUI tests after the final draft/reference changes. MCP's protected OAuth lifecycle is covered.
- Targeted CLI/server/TUI Clippy (`--all-targets --no-deps`) with warnings denied, formatting, architecture boundaries and the commercial-boundary check. The earlier full-workspace Clippy run had unrelated provider warnings; this is not a claim that the entire dirty workspace passes every gate.
- Desktop build/typecheck and 82 unit tests.
- Real Electron workflows: agent colors/filtering, provider artwork and per-model reasoning, file/agent prompt context, hooks, memory, remote server/project isolation with files/events/terminal/drafts, remote provider forms and masked credentials, sandbox settings validation, sharing and revocation.
- `scripts/test-tui-parity.py`: real daemon + tmux, memory conflicts, executing hooks, profile capture/restore, global/project custom provider edits/removal, credential masking, stdio MCP lifecycle, PKCE authorization/cancellation, native scheduled execution/history, agent/reasoning rules, favorites, stash restart, file ranges, agent references and configured shortcuts.
- `scripts/test-tui-connections-security.py`: real daemon + tmux, workspace/outside writes, offline networking, permitted proxy traffic, blocked direct and unlisted traffic, private frozen share previews, preview/link expiry, HTML escaping, revocation, two servers, draft isolation through a full client restart and a real turn through a remote project gateway.
- `scripts/test-tmux.sh`: broad existing terminal regression gate passed. Assertions now accept the displayed Echo model name, verify that Echo does not offer unsupported reasoning levels, and wait for the live command palette after management writes before executing its new command.
- SDK gate: TypeScript and Python sync/async clients each exercised all 105 operations, including management; generated schema checks, typing, lifecycle tests and clean packed-artifact consumers passed.

Retained evidence:

- Expanded management/PKCE tmux run: `/private/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-tui-parity-_akdgogn`.
- Sandbox/sharing/server-switch tmux run: `/private/var/folders/x5/7p86qtnd17gf_c1vvzbc0ygm0000gn/T/haarness-tui-parity-vnhtopbb`.
- Logs: `/tmp/haarness-parity-e2e-final.log`, `/tmp/haarness-connections-security-final.log`, `/tmp/haarness-tmux-final.log`, `/tmp/haarness-desktop-parity-final.log`, `/tmp/haarness-parity-sdks.log`, `/tmp/haarness-tui-final-tests.log`.

All integration fixtures used isolated data/workspace roots and fake credentials. No production provider account, user schedule, user memory or public user conversation was changed by the tests. Changes remain in the existing working tree; no commit or publication was requested.
