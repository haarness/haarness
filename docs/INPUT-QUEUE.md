# Steer and Queue

How messages sent while the agent works are steered into the running turn or queued after it, in Desktop, the TUI, the server API and the SDKs, with the shared setting and its tests. For contributors and for client authors using the input API.

> **User guide:** [Send more while it works](https://haarness.ai/docs/desktop/the-conversation#send-more-while-it-works).

Use **Steer** to correct the task the agent is doing now. Use **Queue** to give it another task after the current work finishes.

For example, while the agent is fixing a bug:

- Send “use the existing helper” normally to steer the next model step. It does not cancel a tool already running.
- Queue “then update the documentation” to wait until the bug fix is finished, including its tool steps and automatic goal continuations.

Queued prompts remain outside the conversation transcript until delivery. They run one at a time in the displayed order, and each runs as its own turn: the agent reads it as a new request, not as a mid-task interruption. A Queue submission while idle starts that prompt immediately.

## Stop, failures and resuming

- **Stop** cancels the current work and dismisses its approval prompts in connected clients. In Desktop, Esc in the empty message box and **File → Stop response** (Cmd+. on macOS, Ctrl+. elsewhere) do the same. If you sent steering the agent has not answered yet, the agent answers it next instead of dropping it.
- After Stop the queue **pauses**, even once that steering has been answered. It also stays idle after a daemon restart.
- If a turn **fails**, prompts queued behind it still run. If a queued prompt's own turn fails, the rest of the queue pauses.
- Resume a paused queue in either of two ways:
  - Choose **Send now** on a pending prompt. That prompt runs, then the rest continue in order.
  - Send a new message. It runs first, then the queue continues.

## Default delivery and project settings

Normal Send uses **Steer** by default. In **Settings → General → Messages sent while working**, choose Steer or Queue: the choice shows what a message sent during work does in the current project, and the line under it says where that value lives and what resetting it leads to. Without a project override it is the global setting (**All projects**), and choosing changes it for every project; **Override for this project** saves the project's own value (the one shown, to start with), after which choices change only this project (**This project**), and **Use global setting (Queue)** removes the override, naming the value the project then falls back to, or **Use parent folder's setting (Queue)** when a parent folder sets it. A value set in a parent folder (**Parent folder**) is changed with an override in this project, since the global setting would not reach it. Without project configuration only the global setting applies.

Both Desktop and TUI support these commands, including during active work:

```text
/input-mode              Toggle Steer/Queue for this project
/input-mode steer        Set this project to Steer
/input-mode queue        Set this project to Queue
/input-mode reset        Remove this project's override
```

In Desktop, `/settings` opens the full settings pane without changing input mode. TUI has no built-in `/settings` command.

The shared preference lives on the connected Haarness server in JSON/JSONC configuration:

```json
{"input": {"delivery": "steer"}}
```

Global `~/.config/haarness/haarness.json{,c}` loads first, then native project `haarness.json{,c}` and `.haarness/haarness.json{,c}` from the Git worktree root down to the selected project directory. Hidden configuration wins over root configuration, and JSON wins over JSONC. With no value, delivery is Steer. This non-executable preference is readable even when project code is not trusted.

Project edits preserve an existing `.haarness/haarness.jsonc` or `.json` and otherwise create `.haarness/haarness.json` in the **selected project directory**. They never change an ancestor or global file. Reset removes delivery overrides from that directory's native JSON/JSONC files, restoring ancestor/global inheritance. Unrelated fields and comments are preserved. Global paths respect Haarness's configured storage locations.

The clients resolve the setting when submitting, so changes from another client or hand edits take effect on the next message without restarting or waiting for an active turn to finish. Existing pending messages retain their delivery choice. Slash commands execute immediately regardless of the selected default. Older servers retain ordinary steering and advertise no input-settings control.

## Desktop

Normal **Send** or **Enter** uses the configured mode. While work runs, the send button's tooltip reads **Steer message** or **Queue message**, so you can see what sending does before you send. **Alt+Enter** explicitly defers a normal message. **Shift+Enter** still inserts a newline.

Queued messages appear in a tray joined to the top of the composer. It is visible only while messages wait. Its header shows the queue's state:

| Header | Meaning |
| --- | --- |
| Queued, with a count | Work is running; the messages run in order after it. |
| Queue on hold · send when you're ready | Nothing runs until you resume the queue. |
| Editing · send to save your changes | You are editing a queued message in the composer. |

Each message has icon buttons: **Edit**, **Send now** (**Steer now** while work runs, which hands it to the current work at its next step, with an arrow of its own), and **Remove**, after which the keyboard moves to the next message's **Remove**, or to the message box. Hovering a row also reveals **Move up** and **Move down**.

**Edit** loads the message's text and attachments into the composer and sets your draft aside. The send button becomes **Save queued message**. Sending saves the message in place: it keeps its position, and a paused queue stays paused. **Esc** or the ✕ in the tray header cancels the edit and restores your draft; it never stops running work. If another client changes, sends or removes that message first, the save is refused and your edit stays in the composer. The tray refreshes after reconnecting and follows the selected server and conversation.

## TUI

**Enter** uses the configured mode; **Alt+Enter** explicitly queues. While work runs, the empty prompt says which: `Enter steers · alt+enter queues`, or `Enter queues until this work is done` in Queue mode. Use `/queue <message>` to defer text, or `/queue` to open the pending list.

Above the prompt, steering the agent has not read yet appears under **Steering · N**. Queued messages appear under **Queued · N**, or **Queue on hold · send when you're ready** when idle. The strip shows the first three messages numbered in order, then a `+N more` line.

Click a queued row, or pick it from `/queue`, for its actions:

- **Steer now** (**Send now** when idle)
- **Edit**
- **Remove**
- **Move up** or **Move down**

**Edit** loads the message into the prompt and sets your draft aside. The queue's header reads `Editing · Enter saves your changes`, and the line `Esc or click cancels and restores your draft` appears under the queue. Enter saves in place; Esc or a click cancels and restores the draft, and never interrupts running work. Esc Esc, Ctrl+C twice and `/stop` stop work the same way as Desktop's Stop.

Slash commands and shell-mode commands are not deferred. Deferred-message actions require a server advertising `input-queue.v1`; older servers retain ordinary steering support.

## API and SDKs

`POST /v1/threads/{threadId}/input` accepts `delivery: "steer" | "queue"`. Omission preserves the previous steering behavior. Example deferred admission:

```json
{
  "requestId": "ea73602e-4b13-4c34-86cd-71cd488fbf61",
  "delivery": "queue",
  "text": "Then update the documentation",
  "attachments": []
}
```

Three optional fields settle how a turn this input starts runs:

- `reasoningEffort` picks the effort: a level, or `default` for the model's own default over a configured `defaults.reasoning_effort` (see [the API](API.md)). Without it, the turn keeps the effort of the chat's latest prompt. Queued prompts, steering that starts a turn, background agent results and the continuation after Stop all keep it, rather than falling back to the default.
- `preview` says whether the client can show file previews. Text clients send `false`. Without it, the turn keeps the chat's last recorded value, so a prompt from a phone or a background result sends the model the same tools and system prompt as the turn before. Changing them would also rewrite the provider's whole cached prompt prefix.
- `attended: true` says a person is at the client and can stop the turn, as Desktop, the terminal UI and an editor over ACP send it. A command's or skill's `` !`…` `` snippet in that turn then runs until it finishes or is stopped, instead of ending at `execution.shell_timeout_ms`. A queued message keeps it until it runs; without it, the turn keeps the deadline.

A deferred admission while busy returns `queued: true`, with no transcript item or new turn yet. Its attachments are checked when it arrives, as a message sent at once is: one the turn would refuse (more than 20 MiB of media, a file that cannot be read) is refused with 400 and not queued, so it never holds back the messages behind it. When its turn comes it starts whatever happened to its files meanwhile; one that has grown past the limit, gone or stopped being a PDF is named in a note the model reads, and the queue goes on. Reuse the request ID when retrying: a deferred admission remembers its first delivery choice even after delivery or removal, so retries do not resurrect work. A person's message steered into a running turn is remembered by its request ID too, so a retry of it returns `queued: true` and is not delivered twice, even when retries race. A steer that started a turn because nothing was running is remembered by that turn: its retry, even one racing it, returns the same `turn` instead of steering the message into it again.

`GET /v1/threads/{threadId}/input/queue` returns `{revision, items}`. Each item has `id`, `text`, and `attachments`, plus the `reasoningEffort`, `preview` and `attended` it was queued with, when they were sent. Mutate through `POST` to the same URL using the latest revision:

```json
{"action":"edit","revision":3,"id":"ea73602e-4b13-4c34-86cd-71cd488fbf61","text":"Update docs and examples"}
```

`edit` may also carry `attachments` to replace the message's attachments; omitting it keeps them. Those it adds are checked the same way and must fit beside those it keeps; those the message already had are not checked again, so an edit that sends them back unchanged is never refused for a file that changed since. Other actions are `remove` with `id`, `steer` with `id`, and `reorder` with `ids` containing every pending ID exactly once. Mutations return the new snapshot. A stale revision or unavailable ID returns HTTP 409; invalid input returns 400. These routes use the same authentication as the conversation APIs. The `input.queue.updated` event carries a complete snapshot; clients should ignore older revisions and refresh after reconnecting.

TypeScript SDK methods are `queueInput`, `inputQueue`, and `changeInputQueue`; Python provides `queue_input`, `input_queue`, and `change_input_queue` in synchronous and asynchronous clients.

Pending prompts and admission tombstones are stored in SQLite. Promotion into the transcript and removal from the pending list happen in one transaction. Each deferred prompt starts its own durable turn after the previous one closes, so it never inherits that turn's step count, permissions or waiting callers.

`POST /v1/threads/{threadId}/interrupt?resume=true` (and `/v1/turns/{turnId}/interrupt?resume=true`) is the interactive Stop. Steering the agent has not answered yet gets a continuation turn that answers it, instead of a "retry it" error. Without `resume=true`, as for Undo and scripts, interrupt only stops. Both forms pause the thread's queue. The SDKs take `{resume: true}` as the query argument of `interruptThreadTree` and `interruptTurn`.

## Verification

The regression suite covers steering priority, intermediate tool boundaries, one-at-a-time delivery, idle and simultaneous admissions, Stop/failure, edits, revisions, ordering, attachment preservation, and admission retries. Run:

```sh
cargo test -p haarness-core -p haarness-store -p haarness-protocol -p haarness-server -p haarness-tui
cargo build -p haarness-cli
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-input-queue-api.py
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-input-queue-tmux.py
HAARNESS_BIN="$PWD/target/debug/haarness" sh scripts/test-sdks.sh
npm --prefix apps/desktop test
npm --prefix apps/desktop run build
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'deferred input queue|Stop answers steering|SSE chat fabric'
```

The HTTP test kills and restarts its own daemon with persisted data. The tmux and Electron tests exercise actual client controls against isolated daemons. Tests use deterministic providers and do not require external model credentials.

The shared setting is available through authenticated `POST /v1/manage/input-settings` on servers advertising `input-settings.v1`. Use `{"action":"get"}` to read effective delivery, scope value, inherited value, source and revision from the response’s `data` field. `set` accepts `delivery: "steer" | "queue" | "inherit"`; `toggle` and `reset` default to Project. Set `scope: "global"` explicitly for global edits. Forms use the returned revision to reject stale saves. The input API's omitted-delivery default remains Steer for SDK compatibility; interactive clients send the resolved delivery explicitly.

Additional setting checks:

```sh
cargo test -p haarness-cli input_settings
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-input-settings-tmux.py
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'shared input settings|deferred input queue'
```
