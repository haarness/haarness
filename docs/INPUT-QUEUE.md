# Steer and Queue

Use **Steer** to correct the task the agent is doing now. Use **Queue** to give it another task after the current work finishes.

For example, while the agent is fixing a bug:

- Send “use the existing helper” normally to steer the next model step. It does not cancel a tool already running.
- Queue “then update the documentation” to wait until the bug fix is finished, including its tool steps and automatic goal continuations.

Queued prompts remain outside the conversation transcript until delivery. They run one at a time in the displayed order. A Queue submission while idle starts the first pending prompt immediately. Stop dismisses cancelled approval prompts in connected clients. Stop, a failed turn, or a daemon restart leaves the remaining queue paused; choose **Send** on a pending prompt to resume. Sending a new ordinary message also starts new work, after which pending prompts can continue.

## Default delivery and project settings

Normal Send uses **Steer** by default. In **Settings → General → Messages sent while working**, choose Steer or Queue. Edits default to **This project**; select **Global** to change the inherited default for other projects. **Use inherited setting** removes the selected scope's override.

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

Normal **Send** or **Enter** uses the configured mode. **Alt+Enter** explicitly defers a normal message. **Shift+Enter** still inserts a newline.

The pending list above the composer lets you:

- Click a message to edit its text without replacing your composer draft.
- Move it up or down, or remove it.
- Choose **Steer** to promote that message into the current task, or **Send** when idle.

Edits retain attachments. If another client changes the queue, a stale save reports a conflict and keeps your text. If the message is sent or removed while editing, the unsaved text stays available to copy. The pending list refreshes after reconnecting and follows the selected server and conversation.

## TUI

**Enter** uses the configured mode; **Alt+Enter** explicitly queues. Use `/queue <message>` to defer text, or `/queue` to open the pending list. Select a message to edit, move, remove, or Steer/Send it. The composer strip separately counts pending guidance and deferred prompts, and shows when deferred work is paused.

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

A deferred admission while busy returns `queued: true`, with no transcript item or new turn yet. Reuse the request ID when retrying: a deferred admission remembers its first delivery choice even after delivery or removal, so retries do not resurrect work.

`GET /v1/threads/{threadId}/input/queue` returns `{revision, items}`. Each item has `id`, `text`, and `attachments`. Mutate through `POST` to the same URL using the latest revision:

```json
{"action":"edit","revision":3,"id":"ea73602e-4b13-4c34-86cd-71cd488fbf61","text":"Update docs and examples"}
```

Other actions are `remove` with `id`, `steer` with `id`, and `reorder` with `ids` containing every pending ID exactly once. Mutations return the new snapshot. A stale revision or unavailable ID returns HTTP 409; invalid input returns 400. These routes use the same authentication as the conversation APIs. The `input.queue.updated` event carries a complete snapshot; clients should ignore older revisions and refresh after reconnecting.

TypeScript SDK methods are `queueInput`, `inputQueue`, and `changeInputQueue`; Python provides `queue_input`, `input_queue`, and `change_input_queue` in synchronous and asynchronous clients.

Pending prompts and admission tombstones are stored in SQLite. Promotion into the transcript and removal from the pending list happen in one transaction. Deferred follow-ups use Haarness's existing logical-turn runner; several follow-ups can share a durable turn ID.

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
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'deferred input queue|SSE chat fabric'
```

The HTTP test kills and restarts its own daemon with persisted data. The tmux and Electron tests exercise actual client controls against isolated daemons. Tests use deterministic providers and do not require external model credentials.

The shared setting is available through authenticated `POST /v1/manage/input-settings` on servers advertising `input-settings.v1`. Use `{"action":"get"}` to read effective delivery, scope value, inherited value, source and revision from the response’s `data` field. `set` accepts `delivery: "steer" | "queue" | "inherit"`; `toggle` and `reset` default to Project. Set `scope: "global"` explicitly for global edits. Forms use the returned revision to reject stale saves. The input API's omitted-delivery default remains Steer for SDK compatibility; interactive clients send the resolved delivery explicitly.

Additional setting checks:

```sh
cargo test -p haarness-cli input_settings
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-input-settings-tmux.py
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'shared input settings|deferred input queue'
```
