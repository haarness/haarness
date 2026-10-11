# Side questions with `/btw`

> **User guide:** [Ask a side question](https://haarness.ai/docs/tui/messages#ask-a-side-question). This page has the engineering detail.

`/btw <question>` asks the active session's provider a question using a snapshot
of the current conversation. It reuses the main request's prepared system,
tool schemas, model settings, and settled conversation prefix, including
context previously added by hooks. An unfinished tool call and everything
after it are omitted until the result arrives. The answer appears in a
temporary TUI panel:

```text
/btw Why did we choose a transaction here?
```

The CLI equivalent is `haarness btw <session-id> <question>`. The REST endpoint
is `POST /v1/threads/{threadId}/btw`, with `{ "question": "..." }`; the response
contains the answer and request-local token usage.

Tool schemas provide context only. Every attempted tool call is refused locally;
the provider gets at most three requests to return an answer. Side questions
never run tools, rerun request hooks, initialize MCP connections, or write user,
assistant, reasoning, usage, or turn records to the main transcript. They do not
interrupt the main turn. Structured-output requirements are relaxed so the side
answer can be plain text.

Prepared requests are held in memory, with at most 16 sessions and a 4 MiB
serialized-size budget per request. A restart, eviction, configuration reload,
changed tool catalog, or incompatible history makes the side question reconstruct
the current project instructions, memory, agent prompt, and eligible schemas.
Transient hook context from an unavailable prepared request cannot be recovered;
hooks are not replayed to recreate it.
