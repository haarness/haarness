# Automatic approvals

> **User guide:** [Permissions and approvals](https://haarness.ai/docs/security/permissions). This page has the engineering detail.

Each chat is in **Auto** or **Review**, independently from `--yolo`.
Auto approves eligible permission requests **once**; it never adds an Always rule or changes the server's permission configuration. Explicit denies, agent permission ceilings and `PermissionRequest` hooks still win. Questions remain manual.

## The server keeps the mode

The mode belongs to the chat, on its server, so every client sees and applies the same one. A TUI, a second Desktop window or a phone can no longer approve a request in a chat someone set to Review, and closing a client does not turn Auto off.

- A chat without its own setting is in Review. Agents a chat delegates to follow it unless they have their own setting; forks and other chats keep their own.
- Review wins down the chain: setting a chat to Review makes its agents ask again even if one was switched to Auto on its own, and an agent under a chat set to Review cannot be switched to Auto by itself (409).
- The server answers eligible requests in an Auto chat without showing a prompt. Requests a tool always asks about, such as starting or deleting Cloud sessions, are never eligible, and neither is a website in the user's Chrome (an explicit request; see [Chrome approvals](PERMISSIONS-AND-EXECUTION.md#chrome-approvals)). Switching a chat to Auto does not release a waiting Chrome request.
- Turning Auto on also approves, once, the eligible requests already waiting in that chat and the agents that follow it.
- Every client hears an `approval_mode.changed` event for the chat and for every agent below it, in the order the changes were made.
- A single turn started with `autoApprove` (a schedule, `haarness run --yolo`, `haarness acp --yolo`, a terminal UI started with `--yolo`) approves its own requests once and teaches no rule to other chats; a shared chat cannot start such a turn.
- A client's `--yolo` never reaches the folder's server, which outlives the client and serves every chat in the folder. While they wait, `haarness run --yolo` and `haarness workflow run --yolo` also answer **Allow once** to what their own chat (its agents, workflows and later turns) asks, and a `--yolo` terminal UI does the same in the conversation it shows; requests that always ask are left to a person. Only `haarness serve --yolo` approves, and records Always rules, for every chat on its server until it stops.

API (`approval-mode.v1` in the server's capabilities):

- `GET /v1/threads/{id}/approval-mode` returns `mode`, the `sourceThreadId` whose setting decided it, whether the chat or a delegating agent has its own setting (`explicit`), and a `revision`.
- `PUT /v1/threads/{id}/approval-mode` with `{"mode":"auto"|"review"|"inherit","revision":…}` changes it. A `revision` that no longer matches answers 409, so a change made from a stale view never overwrites a newer one; omit it to apply the change regardless.

Clients connected to an older server keep the earlier client-side behaviour described under each client below.

## Desktop

- Use the shield control at the left of the composer, which reads **Review** or **Auto**, or search for **Enable Auto approvals** / **Switch approvals to Review**. Its tooltip says what Review asks about by default: edits and commands inside the project run without asking.
- With a conversation open, the control changes that conversation's mode on its server. From a child agent view it applies only to that child and its descendants; the parent and siblings keep their mode. A parent set to Review keeps its agents in Review.
- On a new chat, the control sets the mode for new chats Desktop starts in that workspace/checkout. It applies to chats Desktop creates, not to chats started by the TUI, a phone or the API.
- **Allow once + Auto** answers the visible request first and enables Auto only after the reply succeeds and the view/mode has not changed.
- Per-conversation Auto chosen before servers kept the mode moves to the server the first time Desktop connects to one that supports it.
- Worktrees, remote connections, and projects have separate identities and settings. Enabling Auto in one does not grant Auto to another.
- With an older server, Desktop answers eligible requests itself while its window is open, as before.

## TUI

- **Ctrl+Y** toggles Auto/Review for the open chat. `/permissions-mode auto`, `/permissions-mode review`, and `/permission-mode` work through the command menu too.
- A pending permission offers **Once + Auto** (`u`). Because it changes how the whole chat is answered, the first press only stages it, like the Always choices; Enter or `u` again confirms. It enables Auto only after a successful one-time reply.
- The status band shows the open chat's mode as `approvals: review` or `approvals: auto` when space permits, and follows a change made from another client. A TUI started with `--yolo` shows `approvals: --yolo` instead.
- `/new` starts the new chat in the open chat's mode.
- With an older server, the mode is local to the TUI launch and covers new requests from every chat on that server, as before.

## Verification commands

```sh
npm --prefix apps/desktop test
cargo test -p haarness-tui --lib
cargo test -p haarness-server --test api -- approval auto_approves turning_auto_on delegated_agents
cargo build -p haarness-cli
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-auto-mode.py
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'Auto approvals allow|Auto from a child|workspace Auto stays|SSE chat fabric|dictation inserts|universal search opens settings'
```

Desktop tests drive actual Electron controls against a real Haarness server. The TUI script uses a real tmux terminal and real server with the deterministic test provider; it checks filesystem effects, pending requests, child agents, other chats, deny rules, questions, a change made by another client, daemon restarts and relaunch. No paid model or external side effects are needed.

The event bus serializes cursor allocation through replay and live delivery, so concurrent agents cannot overtake an earlier approval event. Its regression test publishes 8,000 events from eight threads and checks both delivery paths in cursor order.
