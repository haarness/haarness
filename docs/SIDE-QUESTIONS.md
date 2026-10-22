# Side questions with `/btw`

> **User guide:** [Ask a side question](https://haarness.ai/docs/tui/messages#ask-a-side-question). This page has the engineering detail.

`/btw <question>` asks the active session's provider a question using a snapshot
of the current conversation. It reuses the main request's prepared system,
tool schemas, model settings, and settled conversation prefix, including
context previously added by hooks. An unfinished tool call and everything
after it are omitted until the result arrives.

```text
/btw Why did we choose a transaction here?
```

## A side chat

A side question is the start of a side chat: follow-ups keep the thread going,
so "and why?" or "show me the second one" need no repeating. The server keeps
no side chat. Each follow-up carries the thread's earlier questions and answers
(`history`, oldest first, at most 100 and 1 MiB), and the server sends them
after the chat's own history: the first question with the side-question
instructions, its answer, the next question, and so on. A follow-up therefore
starts with the previous question's request bytes and reads back the prompt
cache it wrote. Only the first question carries the instructions, which allow
follow-ups and a longer answer when a question needs one. A side chat always
asks with the chat's own provider, model and agent.

Clients send the finished questions, up to 40 of them and 256 KiB of text. A
longer thread loses its oldest questions 20 at a time, so the first question
sent stays the same from one trim to the next and the prompt cache still holds
between trims; the request right after a trim starts the side chat's part of
the cache afresh (the chat's own part is unaffected), and the side chat no
longer remembers what was dropped.

## API

- `POST /v1/threads/{threadId}/btw` with `{ "question": "...", "history": [{ "question": "...", "answer": "..." }] }`
  answers once the whole answer is in, with request-local token usage.
  `history` is optional.
- `POST /v1/threads/{threadId}/btw/stream` takes the same body and answers with
  an event stream: one JSON `data:` event per step, `delta` (`text` extends the
  answer), `text` (replaces it; an attempt after a refused tool call starts
  over with an empty one), and then `done` (`answer`, `inputTokens`,
  `outputTokens`) or `error` (`code`, `message`, `retryable`, as a failed
  `POST /btw` answers). A missing chat (404), an empty question, or a thread
  with an empty question or answer, more than 100 questions or more than 1 MiB
  of text (400, saying so) fail before the stream starts. A
  client that goes away ends the provider request. A keepalive comment comes
  every 15 seconds. The SDKs read it with `btwStream` (TypeScript) and
  `btw_stream` (Python); servers that have the route list `btw.stream.v1` in
  `/v1/info` capabilities, and clients fall back to `POST /btw` without it.

The CLI equivalent is `haarness btw <session-id> <question>`.

## TUI

`/btw <question>` opens the side chat and asks there; a bare `/btw` opens it,
or brings the thread back. The side chat takes the whole terminal, like
Review: the thread on top, in the transcript's own style (the question as a
prompt block, the answer as a reply that streams as it is written), and a box
of its own for the next question at the bottom.

| Key | What it does |
| --- | --- |
| `enter` | Ask; `shift+enter` (or `alt+enter`, `ctrl+j`) adds a line |
| `esc` | Back to the chat. An answer still being written keeps going, and `/btw` brings the thread back |
| `ctrl+c` | Clear the box; with nothing to clear, stop the answer being written; then back to the chat |
| `/clear` or `/new` (typed in the box) | Start a new thread |
| `/copy` (typed in the box) | Copy the last answer |
| `pgup` / `pgdn`, mouse wheel | Scroll the thread |

Each chat has one side chat for as long as the TUI runs; leaving it keeps the
thread and what you typed. Asking while an answer is still being written is
refused with a notice, and a `/btw <question>` from the chat meanwhile adds the
question to what the box holds. Against a server without `btw.stream.v1` the
answer arrives in one piece and each question is answered on its own, which
the thread says once.

## Desktop

The side chat is a column of its own between the conversation and the side
panel, so Files, Review or the Browser stay open beside it. Its one header row
has a tab per thread, named after its first question, **+** for another, and
at the far end **×** to put it away. The answer streams in the conversation's
own style, with **Copy** on a finished answer, **Retry** under one that failed
or was stopped, and a slim box of its own at the bottom whose send key turns
into **Stop** while an answer is written. Drag the divider on its left to
resize it (arrow keys on the focused divider, double-click to reset); its width
and whether it is open are remembered.

| Action | How |
| --- | --- |
| Ask | `/btw <question>` in the message box: the side chat opens with its box focused. A bare `/btw` just opens it |
| Open the side chat | **⇧⌘J** (Ctrl+Shift+J elsewhere; change it in **Settings → Keyboard shortcuts**; it works from a page in the Browser too, never behind an open dialog), **Open side chat** in **Search** or at the top of the chat's **⋯** menu, the side panel's launcher, or **View → Side chat** |
| Put it away | **×** at the end of its header, or **⇧⌘J** again from its box; either way the keyboard goes back to the message box. `esc` in an idle box goes back without closing it, or puts it away where it covers the workspace |
| Another thread | **+** in its header, or **New side chat** in **Search** |
| Stop an answer | The stop key, or `esc` in its box |
| Close a thread | Its tab's ×, a middle click, or **⌘W** (Ctrl+W elsewhere) while the keyboard is in the side chat (elsewhere ⌘W still closes the window). Closing the last one puts the side chat away |

The conversation keeps at least 480 pixels and the side panel 360; the side
chat gives way first, down to 300. Where the three do not fit, the side chat
and the side panel take turns in one place: whichever was asked for last is in
front (opening the side panel, or a file from a link, brings it forward;
`/btw` or ⇧⌘J bring the side chat back), and the other returns when there is
room. A reload keeps which one was in front. In a window too narrow for the
conversation and the side chat, the side chat takes the whole workspace, as the
side panel does there, until it is put away. A preview the agent opens never
takes its place; one you open yourself does. The new-chat screen has no side
chat: it comes back with the chat.

Threads are kept per chat and survive a reload, as does what was typed in each
box; an answer still being written when the window reloads comes back stopped,
with Retry. They live in files of their own beside Desktop's preferences
(`side-chats/` in its data folder): at most 12 threads and 256 KiB per chat,
the oldest questions giving way first, for the 20 chats whose side chats were
used most recently. Threads with nothing asked and nothing typed are not kept,
and removing a project forgets its side chats. An answer keeps streaming while
another chat is shown and while previews or MCP apps load, and the side chat
shows the chat in front. A chat that has not been started yet has nothing to
ask about: `/btw` there says to send a first message. Against a server without
`btw.stream.v1` the answer arrives in one piece and each question is answered
on its own, which the thread says once.

## Isolation

Tool schemas provide context only. Every attempted tool call is refused locally;
the provider gets at most three requests to return an answer. Side questions
never run tools, rerun request hooks, initialize MCP connections, or write user,
assistant, reasoning, usage, or turn records to the main transcript. They take
no lock and do not interrupt the main turn. Structured-output requirements are
relaxed so the side answer can be plain text.

Prepared requests are held in memory, with at most 16 sessions and a 4 MiB
serialized-size budget per request. A restart, eviction, configuration reload,
changed tool catalog, or incompatible history makes the side question reconstruct
the current project instructions, agent prompt, and eligible schemas. The memory
index, a changed date and earlier turns' hook context are stored in the chat's
history, so the reconstructed question reads them from there; a chat with no
turn yet gets the current memory index and date with the question. Hooks are not
replayed.
