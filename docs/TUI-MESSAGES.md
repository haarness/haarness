# Browse conversation messages in the TUI

> **User guide:** [Find messages and ask side questions](https://haarness.ai/docs/tui/messages). This page has the engineering detail.

Press **Alt+M** or run **`/messages`** to browse the current conversation's
prompts and assistant replies. You can also start with a search:

```text
/messages database migration
```

The browser lists messages newest first, with their author, sequence number,
time and a text preview. Search matches the complete message, including lines
outside the preview, and supports Unicode text and case-insensitive matching. Internal prompts,
reasoning and tool records are excluded. Queued follow-ups appear when they
become part of the conversation. Completed replies arrive through live server
events; an incoming message keeps the selected message stable.

Use Up/Down, Page Up/Page Down, Home/End, or the mouse to choose a message. Enter
opens its actions:

- **Jump to message** positions the transcript at the selected message, using
  its actual wrapped rows. Scrolling remains paused while new output arrives;
  **Ctrl+End** returns to the latest output.
- **Read message** opens its complete text in a scrollable panel. Page Down and
  End reach the rest of a long message.
- **Copy message** copies the original complete text, including line breaks.
  This uses the TUI's existing native/terminal clipboard support.
- **Add text to draft** appends the text without sending it or replacing an
  existing draft. Existing draft attachments remain; attachments belonging to
  the old message are not copied. Leave shell mode before using this action.
- **Fork from message** creates a new conversation before a selected user
  prompt, or through a selected assistant reply. Later messages are excluded.
  The original conversation and its draft remain available under `/sessions`.
- **Back to messages**, or Escape from the actions, restores the search and
  selected message. Escape from the browser or read panel returns to chat;
  Alt+M restores the browser position in that conversation.

Browsing does not undo anything. `/timeline` continues to select folder
checkpoints for undo. Child-agent conversations expose only read actions.
Actions validate the current conversation and message identity before they run,
so a stale selection cannot affect a different conversation.

The same browser works in a local TUI and a TUI attached to a server. Alt+M uses
the configurable `messages` shortcut under `[tui.keybinds]`.

## Move between messages

**Alt+Up** and **Alt+Down** move the transcript to the previous or next
message: the nearest prompt or reply that starts above, or below, the top row
of the view becomes the top row, as **Jump to message** places it.
**Alt+Shift+Up** and **Alt+Shift+Down** do the same for your prompts only,
**Alt+Home** goes to the start of the conversation (as Ctrl+Home does) and
**Alt+End** to your last prompt. A message counts when it has text of its own:
a reply that is only tool calls, a prompt that is only attachments and internal
prompts are passed over. With no message that way, the view moves a page.
Moving pauses following like any scroll; End or Ctrl+End returns to the latest
output.

The keys work with a draft in the prompt (Alt+Left/Right still move by word),
in child-agent views, and while an approval card or a question waits: those
keep every other key, and a message key never answers them. They are the
configurable shortcuts `messages_previous`, `messages_next`,
`messages_previous_user`, `messages_next_user`, `messages_first` and
`messages_last_user` under `[tui.keybinds]` or in `/keybinds`; a shortcut
may name `up`, `down`, `left`, `right`, `home`, `end`, `pageup` or `pagedown`
with ctrl, alt or shift.

A shortcut you chose wins over a default on the same key: `sessions = "alt+up"`
keeps Alt+Up for the session list and leaves `messages_previous` without one.
A setting that cannot apply (an unknown action, a key that cannot be bound, a
key another of your shortcuts already has) is left out on its own while the rest
apply, and the status line names what was left out once at launch. `/keybinds`
saves only the shortcuts that differ from config.toml and the defaults, so a
TUI from before these keys still reads the file.

Alt with an arrow reaches the TUI wherever Option or Alt reaches the terminal
at all. iTerm2 and other xterm-style terminals send Alt+Up as `ESC [1;3A`;
macOS Terminal sends Option+Up as `ESC ESC [A` (Escape, then the arrow), which
the TUI reads as the same key rather than as Escape; tmux passes both on as
they are. Ctrl with an arrow was not used because macOS keeps Ctrl+Up and
Ctrl+Down for Mission Control.

## Verification

```sh
cargo test -p haarness-tui
cargo build -p haarness-cli
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-message-browser-tmux.py
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-tui-navigation-tmux.py
```

The terminal test creates a private Git repository and server data directory. It
uses the real TUI, HTTP API and deterministic Echo provider. It checks multiline
Unicode search/copy, read and jump actions, existing draft preservation, live
updates, exact persisted fork boundaries, a fresh conversation, terminal resize,
and a separate attached client. It restores the macOS clipboard and stops only
its own tmux sessions and server. Captures and a result report are written to
`/tmp/haarness-message-browser`, configurable with `MESSAGE_BROWSER_EVIDENCE`.

## Cards and the status line

The terminal shows a card (a top-right toast) only for things that need the
reader: an approval or question, an error, a lost connection, a goal or
workflow that stopped, and work that finished while the terminal was not in
front (a task finished in another session, or in this one while the window
was unfocused). Ambient facts — input cleared, text copied, a provider
connected, a reconnect, a task being stopped — are written to the status line
under the composer instead. Desktop notifications follow the same rule: they
are sent only while the terminal is unfocused.

The **Connection lost · retrying** card shows at most once every 30 seconds,
so a proxy that keeps dropping the event stream cannot make it flicker; the
status line says **reconnecting** while the connection is down, and
**Reconnected** as soon as the server sends anything again, a keepalive
included. The server sends one every 15 seconds: a stream that brings nothing
for 45 seconds died without closing (the computer slept, the network changed,
a proxy froze) and is followed again from its last event, as is a terminal
socket that has brought nothing, its pings' answers included, for as long. A
stream counts as working again only once it has stayed open 10 seconds or
brought an event; until then its retries keep backing off, up to 5 seconds.
If the task that follows the events ends by itself (a fault in the TUI), it
is started again from a fresh snapshot and clears the indicator once the server
answers; if it keeps ending (three times in a minute), the status line says
**not following the server · trying again in a minute, or restart Haarness**
and nothing claims to be reconnecting until it tries again.

"Finished" means the turn completed. A **finished** card and banner wait half a
second (`state::completion`, the same grace as Desktop) because the daemon
starts the next queued prompt as a new turn moments after the last one ends; if
the conversation starts another turn inside that grace, nothing is announced,
and the last turn of a queue is judged by how long the whole queue took (work
of 20 seconds or more in the open conversation earns one). A failed turn says
**Haarness failed: …** at once (a card only while unfocused, or for another
session), and a stopped turn says nothing. A turn that completes or fails while
work it started in the background (a child agent, a workflow run) still owes
the conversation a result says nothing either and drops a held card: that work
delivers its result as a new turn, which is announced, judged by how long the
whole run took. Only that delivered result carries the waiting run on: any
prompt from the person in the meantime, whether the work was stopped or still
runs, starts a run of its own.

A custom status-line command that keeps failing logs one WARN per distinct
error, DEBUG while the same error repeats, and INFO when it recovers.
