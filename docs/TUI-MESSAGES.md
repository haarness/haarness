# Browse conversation messages in the TUI

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

## Verification

```sh
cargo test -p haarness-tui
cargo build -p haarness-cli
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-message-browser-tmux.py
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
