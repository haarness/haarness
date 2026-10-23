# Automatic approvals

> **User guide:** [Permissions and approvals](https://haarness.ai/docs/security/permissions). This page has the engineering detail.

Each chat is in **Auto** or **Review**, independently from `--yolo`.
In Auto, a reviewer model answers the eligible permission requests a person would see in Review: it approves a request **once**, blocks it with a reason the agent reads, or leaves it to a person with a short note (see [Auto's reviewer](#autos-reviewer)). Auto never adds an Always rule or changes the server's permission configuration. Explicit denies, agent permission ceilings and `PermissionRequest` hooks still win; a hook's Always answer counts once in a chat in Auto. Questions remain manual.

## Auto's reviewer

- **What it answers.** Rules, remembered answers, `PreToolUse` and `PermissionRequest` hooks and extensions decide first; the reviewer sees only what is left to ask. Haarness's own asks are reviewed: folders outside the project, secret-named reads, agent switches, generated media. So is an ask that covers a whole tool (`"bash": "ask"`, `"edit": "ask"`, `"*": "ask"`), which says how strict Review is. **An ask you or the project configured for something narrower (`"bash": {"git push *": "ask"}`) goes to a person in Auto**, as a request you asked to see, and is never approved once. Requests only a person answers never reach the reviewer: Cloud actions that spend money or delete, a site in your Chrome, an edit of Haarness's own configuration files, and a shell line Haarness could not read.
- **What it decides.** Allow approves once, never Always and never a rule. Deny refuses the call; the agent reads "Auto review blocked this action (<rule>): <reason>. Do not try to reach the same result another way; ask the user if it is needed." Ask shows the ordinary prompt with the reviewer's note in it. A failed review (an error, no model answering, the deadline) asks a person with a note that Auto review was unavailable; it never approves.
- **What it sees.** Your messages (each up to 2,000 characters, a slash command's template labelled as not yours), the agent's tool calls with their arguments (the newest 30, each up to 300 characters), the task a delegating agent gave a subagent, and the pending action whole: its permission, patterns and the tool call that raised it, with the working directory and git branch; about 24,000 characters in all. Never the agent's prose, tool results, or internal and compaction messages. Everything it sees is data: only your messages count as authorization. An action longer than 16,000 characters is not reviewed; it goes to a person.
- **How Haarness checks it.** The reviewer rates risk (low, medium, high, critical) and authorization (unknown, low, medium, high) as well as stating an outcome; Haarness keeps the stricter of the outcome stated and the one the ratings call for, reads a reply that misses the format cautiously (a missing rating counts as high risk and unknown authorization), and turns an approval into an ask when a shell command deletes (`rm`, `rmdir`, `unlink`) a path outside the project that none of your messages names. After 3 blocks in a row, or 20 in one turn, reviewing pauses for the rest of the turn and requests go to a person with a note saying so.
- **Which model.** `auto_review.model` when set; otherwise `small_model`, then the chat's own model when that fails. Never another provider's model unless you configured one. A small model that fails a review twice is skipped for 10 minutes. Each review is one structured call at temperature 0 with a bounded answer, a second call when the first fails or has not answered after 10 seconds, and a deadline of 60 seconds (`auto_review.timeout`). Stop ends the review with the turn.
- **Cache.** An approval covers the same tool call's later requests (a command's folder outside the project, then the command) for 10 minutes, in memory only.
- **Mode changes.** A request under review is registered but not shown; no lock is held while the model answers. When the review ends, Haarness settles it in the mode that holds then: a switch to Review while the review runs ends the review at once and shows the prompt, and nothing is approved after Review. Turning Auto on reviews the eligible requests already waiting instead of approving them outright; one the reviewer leaves to a person stays shown, and a person can answer it meanwhile.
- **Turn it off.** `"auto_review": {"enabled": false}` makes Auto approve every eligible request once, as before the reviewer existed, configured asks included.

```jsonc
// ~/.config/haarness/haarness.json
{
  "auto_review": {
    "enabled": true,                       // default
    "model": "anthropic/claude-haiku-4-5", // default: small_model, then the chat's model
    "environment": ["origin is our GitHub organization; main is protected"],
    "allow": ["Pushing feature branches to origin"],
    "block": ["Deploys and package publishing"],
    "timeout": 60                          // seconds
  }
}
```

`environment`, `allow` and `block` are plain-language lines the reviewer reads as your settings. A project's `haarness.json` may only add `block` entries, which the reviewer reads as untrusted text from the repository that can only make it stricter, and lower `timeout`; anything else it sets under `auto_review` is ignored with a notice in the log. The reviewer's instructions are the `auto-review` [prompt file](PROMPT-FILES.md). `haarness doctor` names the model that reviews.

## The server keeps the mode

The mode belongs to the chat, on its server, so every client sees and applies the same one. A TUI, a second Desktop window or a phone can no longer approve a request in a chat someone set to Review, and closing a client does not turn Auto off.

- A chat without its own setting is in Review. Agents a chat delegates to follow it unless they have their own setting; forks and other chats keep their own.
- Review wins down the chain: setting a chat to Review makes its agents ask again even if one was switched to Auto on its own, and an agent under a chat set to Review cannot be switched to Auto by itself (409).
- The server answers eligible requests in an Auto chat through its reviewer, without showing a prompt unless the reviewer leaves one to a person. Requests a tool always asks about, such as starting or deleting Cloud sessions, are never eligible, and neither is a website in the user's Chrome (an explicit request; see [Chrome approvals](PERMISSIONS-AND-EXECUTION.md#chrome-approvals)) or an edit of one of Haarness's own configuration files (see [Haarness's own configuration files](PERMISSIONS-AND-EXECUTION.md#haarnesss-own-configuration-files)). Switching a chat to Auto does not release a waiting Chrome request.
- Turning Auto on also reviews the eligible requests already waiting in that chat and the agents that follow it (with the reviewer off, it approves them once).
- Every client hears an `approval_mode.changed` event for the chat and for every agent below it, in the order the changes were made.
- `haarness --auto` (and `haarness tui --auto`, `haarness attach --auto`) opens its chat (new, `--continue`d or `--session`) in Auto on the server, as Ctrl+Y would; switching to another folder from that TUI does not carry it there. `haarness run --auto` puts the chat it starts or continues in Auto before anything runs in it, so the server answers the run's eligible requests, its agents' and its workflows'; a request that always asks, or one the reviewer leaves to a person, is still asked at the terminal with the reviewer's note, or with no terminal stops the run, printing the reviewer's reason and the rule that would allow it. Either way the chat stays in Auto afterwards for every client until someone switches it back, which is what sets `--auto` apart from the per-run `--yolo`; the two cannot be combined. A server without `approval-mode.v1` refuses `run --auto` before creating a chat; a TUI on such a server falls back to its own switch, as Ctrl+Y does.
- A single turn started with `autoApprove` (a schedule, `haarness run --yolo`, `haarness acp --yolo`, a terminal UI started with `--yolo`) approves its own requests once and teaches no rule to other chats; a shared chat cannot start such a turn. Its agents follow it, and so do the tool and shell calls of a workflow it starts (`ctx.tool`, `ctx.shell`), also after the turn ends; a person's own `!` command or browser tool call in that chat is still asked.
- A scheduled run never waits for a person. Besides what its turn approves, the workflows it started keep approving once after the turn ends, and a request that always asks (a Cloud action that spends money or deletes, a website in the user's Chrome) is refused at once with the reason, so the run goes on and its next slot is not skipped. This holds only for the run's own turn and the work it starts: a person who later types in the run's chat, runs `!` there or forks it is asked as usual, and so is a workflow their turn starts.
- A client's `--yolo` never reaches the folder's server, which outlives the client and serves every chat in the folder. While they wait, `haarness run --yolo` and `haarness workflow run --yolo` also answer **Allow once** to what their own chat (its agents, workflows and later turns) asks, and a `--yolo` terminal UI does the same in the conversation it shows; requests that always ask are left to a person. Only `haarness serve --yolo` approves, and records Always rules, for every chat on its server until it stops.
- `haarness run` without a terminal (a script, a CI job) cannot ask: on a request it stops the run and says which rule would allow it and where it goes, for example "Stopped: the agent asked to run a command (git push origin main), and nobody can approve that in this run. Allow it by adding {"permission":{"bash":{"git push *":"allow"}}} to haarness.json in the project or ~/.config/haarness/haarness.json, or run again with --yolo to approve every request." The example is the rule an **Always** answer would add; agent switches and the loop guard get no example.

API (`approval-mode.v1` in the server's capabilities):

- `GET /v1/threads/{id}/approval-mode` returns `mode`, the `sourceThreadId` whose setting decided it, whether the chat or a delegating agent has its own setting (`explicit`), and a `revision`.
- `PUT /v1/threads/{id}/approval-mode` with `{"mode":"auto"|"review"|"inherit","revision":…}` changes it. A `revision` that no longer matches answers 409, so a change made from a stale view never overwrites a newer one; omit it to apply the change regardless.
- `approval-review.v1` is in the capabilities while the reviewer is on. A request the reviewer left to a person carries `review: {status, risk?, rule?, rationale?}` (status `asked`, `failed` or `paused`) on `GET /v1/permissions`, on `permission.requested` and on the stored `permission_request` item. A call it blocked ends with a tool result whose `metadata.review.status` is `blocked`, so clients tell it from a person's reject. While a review runs, `turn.status` reads `reviewing` with the tool and call it is about.

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
- `haarness --auto` opens the chat in Auto (see above).
- The status band shows the open chat's mode as `approvals: review` or `approvals: auto` when space permits, and follows a change made from another client. When a chat's mode cannot be read (the server did not answer after a switch), it reads as Review until the server says otherwise, never as the previous chat's mode. A TUI started with `--yolo` shows `approvals: --yolo` instead.
- `/new` starts the new chat in the open chat's mode.
- With an older server, the mode is local to the TUI launch and covers new requests from every chat on that server, as before.

## Verification commands

```sh
npm --prefix apps/desktop test
cargo test -p haarness-tui --lib
cargo test -p haarness-server --test api -- approval auto_approves turning_auto_on delegated_agents auto_review::
cargo build -p haarness-cli
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-auto-mode.py
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'Auto approvals allow|Auto from a child|workspace Auto stays|SSE chat fabric|dictation inserts|universal search opens settings'
```

Under the deterministic test provider (`HAARNESS_TEST_PROVIDER=1`) the reviewer answers from `HAARNESS_TEST_AUTO_REVIEW` (`allow`, the default, `ask`, `deny`, `fail`, `slow` or `stall`), or from a `[review:<verdict>]` marker after a scripted call; real reviews never read either.

Desktop tests drive actual Electron controls against a real Haarness server. The TUI script uses a real tmux terminal and real server with the deterministic test provider; it checks filesystem effects, pending requests, child agents, other chats, deny rules, questions, a change made by another client, daemon restarts and relaunch. No paid model or external side effects are needed.

The event bus serializes cursor allocation through replay and live delivery, so concurrent agents cannot overtake an earlier approval event. Its regression test publishes 8,000 events from eight threads and checks both delivery paths in cursor order.
