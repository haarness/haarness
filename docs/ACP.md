# Agent Client Protocol

> **User guide:** [Use Haarness from your editor](https://haarness.ai/docs/server/acp), with editor
> set-up. This page is the engineering view.

`haarness acp` lets an editor that speaks the [Agent Client Protocol](https://agentclientprotocol.com)
use Haarness as its agent. The editor starts the command and talks newline-delimited JSON-RPC over
its stdin and stdout. The adapter is a client of the folder's ordinary Haarness server, not a second
runtime: it starts or reuses that server, and every editor session is a durable Haarness chat that
the TUI, Desktop and the API also see. Closing the editor ends the bridge, never the server's work.

```sh
haarness acp                                   # the current folder
haarness acp --workspace ~/src/app -m openai/gpt-5.6 --agent build
haarness acp --yolo                            # approve requests for this bridge (not Chrome sites)
```

`--provider`, `--model` and `--agent` apply to sessions the editor creates; a loaded session keeps
its own.

## Protocol surface

Implemented in `crates/haarness-cli/src/acp.rs` on the `agent-client-protocol` crate, protocol
version 1.

| Direction | Messages |
| --- | --- |
| Editor → Haarness | `initialize`, `session/new`, `session/load`, `session/prompt`, `session/cancel` |
| Haarness → editor | `session/update` (text, reasoning, tool calls and results, plans), `session/request_permission`, `$/cancel_request` |

`initialize` advertises `loadSession`, image, audio and embedded-context prompts, and MCP over HTTP and SSE
(stdio is always supported).

- **Sessions.** A session id is the Haarness chat id. `cwd` must be absolute and resolve to the
  workspace the bridge was started for; `additionalDirectories` is refused, so an editor cannot point
  a running server at another checkout. `session/load` replays the chat's stored items as updates.
- **Prompts.** Text blocks become the message; image and audio blocks, and embedded PDF, image, audio
  or video resources, become attachments; an embedded text resource and a resource link are added to
  the text with their URI. Other blocks are refused. A turn that completes ends with `end_turn`, a
  stopped one with `cancelled`, and a failed one as an error. A prompt the server refuses (the chat
  already has a turn running, started in another window or run; a hook blocks the prompt) is an
  invalid request (`-32600`) whose message is the server's own sentence (`prompt blocked by a hook:
  <reason>` for a hook's block); a turn running elsewhere runs on.
- **MCP servers** the editor passes are added to the server as `acp-<chat id prefix>-<name>`; a stdio
  server's command must be an absolute path.
- **Approvals** offer *Allow once*, *Allow while Haarness is running* (only when the request allows
  more than once; *Allow in this chat* for a site in the user's Chrome) and *Reject*. The editor is
  asked one request at a time. A request settled first somewhere else (another app, a sibling's
  answer) is withdrawn with `$/cancel_request`.
- **Subagents.** Tool calls and approvals of the subagents and workflow agents a prompt starts arrive
  on the editor's session while that prompt runs, titled `<subagent> · <call>`, with
  `_meta.haarness.childSessionId` and `childTitle`.
- **Tool kinds** map from Haarness tools: `read`/`list` → read, `write`/`edit`/`multiedit`/
  `apply_patch` → edit, `glob`/`grep` → search, `bash` and `bash_stop` → execute, `bash_output` →
  read, `webfetch` → fetch, `todowrite`/`task`/`goal` → think, anything else → other. A todo list is
  sent as a plan.
- **Commands in the background.** The bridge does not follow background work up, so an agent's
  foreground command never moves to the background by itself; one the agent starts there with
  `run_in_background` stops after `execution.shell_timeout_ms` (or the agent's own `timeout`, when it is not 0), and
  its end reaches the chat as a turn of its own on the server.

The rest of the behaviour (Auto review notes in titles, requests raised between prompts, reconnects)
is described under [Servers and clients](REFERENCE.md#servers-and-clients), and the design rule under
[Architecture](ARCHITECTURE.md#extension-surfaces).

## Tests

```sh
cargo test -p haarness-cli acp                    # unit tests in acp.rs
cargo test -p haarness-cli --test acp_e2e         # a real bridge and server with the echo provider
```

`acp_e2e` drives `haarness acp` over stdio: session creation, tools, permissions and durable reload;
subagent requests named after the subagent; requests withdrawn when settled elsewhere; requests raised
between prompts; and Auto review blocks, live and on reload.
