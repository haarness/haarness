# Observability

Haarness keeps its existing structured daily logs and can additionally export traces through native OpenTelemetry OTLP. Export is disabled unless an endpoint is configured, so a normal local invocation does not contact a collector. Trace export is separate from [usage statistics](USAGE-STATISTICS.md), the daily summary Haarness shares with Haarness Cloud unless you turn it off.

## Enable OTLP traces

Haarness supports OTLP over HTTP with binary protobuf. The generic endpoint gets the standard `/v1/traces` suffix:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
OTEL_SERVICE_NAME=haarness-dev \
haarness serve --port 4096
```

Use `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` when the complete traces URL should be used exactly. Signal-specific variables take precedence over their generic equivalents. Supported OpenTelemetry settings include:

- `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
- `OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_EXPORTER_OTLP_TRACES_HEADERS`
- `OTEL_EXPORTER_OTLP_TIMEOUT` and `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT`
- `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES`
- `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_EXPORT_TIMEOUT`, `OTEL_BSP_MAX_QUEUE_SIZE`, and `OTEL_BSP_MAX_EXPORT_BATCH_SIZE`
- `OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG`

Supported sampler names are `always_on`, `always_off`, `traceidratio`, `parentbased_always_on`, `parentbased_always_off`, and `parentbased_traceidratio`. The default is `parentbased_always_on`. `OTEL_PROPAGATORS` accepts `tracecontext` (the default), `baggage` and `none`. `OTEL_EXPORTER_OTLP_COMPRESSION=gzip` compresses exports. Set `OTEL_SDK_DISABLED=true` to disable export even when an endpoint remains in the environment.

A setting Haarness cannot use never fails a command, because shells often set `OTEL_*` variables for other tools. Haarness reports it and carries on: on a terminal it prints a `haarness:` line on stderr, and it always writes a warning to the log.

| Setting | What happens |
| --- | --- |
| `grpc` or `http/json` protocol | Exports with http/protobuf (the only transport this binary ships) |
| Unknown sampler or a ratio outside 0–1 | Uses `parentbased_always_on` or a ratio of 1 |
| Unknown propagator (`b3`, `jaeger`, `xray`) | Ignored; `tracecontext` and `baggage` still work |
| An endpoint that is not an `http(s)` URL with a host | Trace export stays off |
| Anything the exporter rejects (an unsupported compression, malformed headers) | Trace export stays off, with the exporter's reason |

When the collector cannot be reached or refuses an export, the SDK's error is written to the log, and a command on a terminal ends with `haarness: traces could not be exported: …`. A command that ran out of time flushing says the last spans may be missing.

`HAARNESS_LOG` and `RUST_LOG` continue to control file/stderr logs. They do not suppress OTLP spans. Use `HAARNESS_OTEL_FILTER` for an independent `tracing-subscriber` filter; its default records `info` and above for Haarness crates and `warn` and above elsewhere.

### The server keeps the settings it started with

Clients reuse the project's running server (daemon), and an OTLP exporter cannot be reconfigured in a running process. The server records the endpoint it exports to in its discovery file. When a command with an endpoint configured reuses a server that exports elsewhere, or not at all, it says so once and names the fix: `haarness serve --stop`, then run the command again. Haarness never restarts the server for you, since that would cancel turns other windows are running. The reverse also holds: a server started with OTEL settings (for example by a Desktop launched from such a shell) keeps exporting for every client until it stops.

## Span contract

The exported resource includes `service.name` (from `OTEL_SERVICE_NAME`, else `service.name` in `OTEL_RESOURCE_ATTRIBUTES`, else `haarness`), `service.version`, `service.instance.id` (random per process), `process.pid`, `process.executable.name`, `haarness.process.role` (`cli`, `tui` or `server`), the OpenTelemetry SDK attributes and the rest of `OTEL_RESOURCE_ATTRIBUTES`.

One `haarness run` is one trace. The CLI and the TUI send W3C `traceparent` with every request to the server, and the server keeps that context across the background work a request starts:

```text
haarness run                                  INTERNAL  CLI process
├─ POST /v1/threads                           SERVER
└─ haarness.client.turn                       CLIENT
   └─ POST /v1/threads/{id}/turns             SERVER
      └─ start_turn
         └─ invoke_agent build                agent.turn
            ├─ chat {model}                   provider.request, CLIENT
            ├─ execute_tool read              tool.call
            ├─ execute_tool task              tool.call
            │  └─ invoke_agent general        the subagent's turn
            │     └─ chat {model}
            └─ chat {model}
```

- `haarness.command` (named `haarness <command>`): one finite command, with `haarness.command.name` and `haarness.workspace.id`. Long-lived processes (`serve`, the TUI, `attach`, `desktop`, `acp`, `remote`) have no command span: a span lasting hours is lost when the process is killed. Their traces start per request; the TUI adds a `haarness.client.turn` span when it sends a prompt.
- `http.request` (named `{METHOD} {route}`): server requests, including remote host routes, with `traceparent`/`tracestate` extraction, `http.request.method`, `http.route` (the route template, never the raw path, which carries ids and share tokens), `http.response.status_code` and server span kind. 5xx responses are errors. Polling reads (`GET /v1/turns/{id}`, `/v1/questions`, `/v1/permissions`, `/v1/turns/active`, `/v1/events` and `/healthz`) are debug-level, so the default filter leaves them out.
- `agent.turn` (named `invoke_agent {agent}`): one agent turn, with `gen_ai.agent.name`, `gen_ai.conversation.id` (the chat), `haarness.turn.id`, `gen_ai.provider.name`, `gen_ai.request.model` and `haarness.agent.depth` (0 for the chat's own agent).
- `provider.request` (named `chat {model}`): one model request, with `gen_ai.*` provider and model, `gen_ai.response.finish_reasons`, and the integer token counts `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.usage.cache_read.input_tokens` and `gen_ai.usage.cache_creation.input_tokens`. Each retry is its own span.
- `tool.call` (named `execute_tool {tool}`): one tool call, with `gen_ai.tool.name` and `gen_ai.tool.call.id`. Parallel reads and `task` calls stay under their turn; a `task` call's subagent turn is its child.
- `workflow.run`, `chat.title` and `context.compact`: a workflow run, chat titling and context summarization, each in the trace that started it.

A failed turn, model request or tool call has status ERROR with `error.type` set to a stable category (`provider_error`, `provider_blocked`, `provider_incomplete`, `tool_error`, `permission_denied`, …). The error message is not exported. A stop the person asked for is not an error.

The batch processor is flushed during ordinary process shutdown, including graceful `serve` shutdown.

## Logs and log levels

Every Haarness process using a data folder (the terminal UI, the workspace servers behind it and Desktop, one-shot commands) writes to the same daily file, `<data>/logs/haarness.log.<date>`; `haarness logs` lists the files and `haarness logs --tail 50` prints the newest lines. Each line names the process that wrote it after its time: its process id and role, the command it runs (`serve` for a workspace server, `tui`, `run`, `acp`, `desktop`, …):

```text
2026-10-18T07:12:03.418Z 48211 serve  INFO haarness_core::runtime: reporting a background task the restart stopped …
```

A workspace server started in the background (by `serve --daemon`, the terminal UI or Desktop) has no stderr. If it stops before it is listening, it writes why (its error, or a panic) to `<data>/logs/server-start.log` as one line marked with an id its launcher chose, and its launcher adds that to the error it reports: `Haarness server exited during startup (exit status: 1): Port 4096 on 127.0.0.1 is already in use. …`. Once listening, a server writes nothing more there. A launcher moves a file past 64 KiB to `server-start.log.old`, and reads the end of both.

A process starts at `info` for the Haarness crates and `warn` elsewhere, or at `--log-level` (`trace`, `debug`, `info`, `warn`, `error`) or `HAARNESS_LOG` / `RUST_LOG`. `--log-level` also applies to the workspace server that command starts; a server that is already running keeps its level.

To see more from processes that are already running, without restarting a server (which stops its turns), raise the level for a while:

```sh
haarness logs level debug --for 30m   # 30m by default, at most 4h
haarness logs level info --until-stopped
haarness logs level                    # what is in force, and until when
haarness logs level stop
```

The level lives in `<data>/log-level.json`. Each process reads it about every 2 seconds, logs `log level raised for a while` (or `… until stopped`) when it follows, and goes back to its own level, logging `log level back to this process's own`, when the time is up or the level is stopped. A level kept until stopped also applies to processes started later. A raised level only ever makes a process log more: it raises the Haarness crates that log less than it and keeps the rest of the process's own filter (from `--log-level`, `HAARNESS_LOG` or `RUST_LOG`), so `haarness logs level info` never quiets a server started with `--log-level debug`, and `haarness logs level debug` under `RUST_LOG=info,hyper=trace` gives Haarness debug while hyper keeps trace.

`haarness logs export` packs the log files into `haarness-logs-<time>.zip` in the current folder (`--output` to choose), newest first up to 200 MB, with a `README.txt` saying what it holds. Known credential shapes are replaced with `[redacted]` (Authorization and cookie values, bearer tokens, credential-named `key=value` and `--key value` pairs, URL user info and credential query parameters, JWTs, well-known token prefixes), and private keys (`-----BEGIN … PRIVATE KEY-----` to `-----END … PRIVATE KEY-----`, PGP's `PRIVATE KEY BLOCK` included, on one line or over several) with `[redacted private key]`; a key whose end line was cut off is left out up to the next log line, or for at most 100 lines, and marked `[the redacted private key had no end line]`; text you typed or a tool printed can still hold a secret, and paths, host names and chat ids stay. `--full` packs the files as they are.

## Diagnostic log lines

These lines are designed to be safe to keep in daemon logs (they are not exported with traces):

- `hook command failed` (WARN): a hook exited non-zero (other than a clean exit-2 block) or was killed, including `async` hooks. Fields: `event`, `session` (the chat it ran for) when the event has one, `tool` when the event has one, `program`, `code` or `signal`, `asynchronous`, and the start of `stderr` (up to 300 characters). Such a failure never blocks the action.
- `hook blocked` (INFO): exactly once per block from `PreToolUse`, `PermissionRequest` or `UserPromptSubmit`, whether by exit code 2, a JSON `block`/`continue: false`, or a permission `deny`. Fields: `event`, `session`, `tool`, `program`, `code` and the first line of the `reason` (up to 200 characters). A block from a command that also failed logs both lines. Observational events cannot block, so their block answers log only at DEBUG.
- `hook timed out`, `hook failed to spawn` and `hook failed` (WARN) carry the same `event`, `session`, `tool` and `program` fields.
- `request rejected` (WARN): the API refused a request with 401 or 403. Fields: `method`, `path` without the query string, `reason` (`missing`, `wrong_scheme`, `bad_credential` or `forbidden` for a host/origin check), the Authorization `scheme` name only (`Bearer`, `Basic`, … or `unrecognized`), `remote` address and a clipped `user_agent`; a `forbidden` line also names the `origin` host (`none` without an Origin, `opaque` for `null`) and the `fetch_site` the browser sent. At most one line per path prefix (for example `/v1/threads`) and reason is written each minute; the next line reports how many were `suppressed`.
- `command failed` (ERROR): a command ended with an error, for example a workspace server that could not start. Fields: `command` and the redacted `error` chain (up to 2,000 characters).
- `haarness starting` (INFO): every command's first line. Fields: `version`, `commit` (the git commit the binary was built from, `<sha>` or `<sha>-dirty`, or `unknown` for a build outside this repository's own git checkout), and `workspace`. `-dirty` reflects the tree when the build script last ran, which happens when HEAD moves.
- `hot reload source scan failed` (WARN): a watched configuration source could not be read, with the path in `error`. The watcher keeps polling; repeats of the same error log at DEBUG, a different error warns again, and `hot reload source scan recovered` (INFO) follows once the sources read again.
- `provider safety check stopped the response` (WARN): a provider ended a response with a content-filter, guardrail or refusal stop. Fields: `provider`, `stop_reason` as the provider sent it, and the provider's own `request_id` (from the `request-id`, `x-request-id` or `x-amzn-requestid` response header) and `response_id` (the message or response id) when it sent them, so the provider can look the response up. Only identifier-like values are logged; anything else reads `unknown`.
- `skipping provider-incompatible tool schema for Claude` (WARN): a tool was left out of Claude requests because Claude rejects its root input schema. Fields: `tool`, `issues`. Once per tool per process.
- `truncated MCP tool description` (DEBUG): fields `tool`, `original_chars` and `limit`.
- `panic: <message>` (ERROR): Haarness code panicked, in any thread or task, with `location` (`file:line:column`) and the `thread` name; the message is clipped to 500 characters. A background server's stderr goes nowhere, so this line is the only record. A panic inside a turn also logs `a turn stopped on a panic` and ends that turn as failed with "Haarness stopped this turn after an internal error (…)", so the chat takes the next message instead of staying busy until the server restarts.
- `config: skipped <command|agent|skill> <path>: <error>` (INFO, at startup and after each applied reload): a definition file whose frontmatter does not parse, or is written in a language other than YAML or JSON (`JavaScript frontmatter is not supported`, `frontmatter language "coffee" is not supported`), was left out. `skipped skill whose frontmatter does not parse` (WARN, fields `path` and `error`) is the same report from the skill catalog. The language is quoted redacted and clipped to 32 characters, since the rest of the opening line can hold anything.
- `approved once (Auto)`, `approved once (unattended run)` and `auto-approved (--yolo)` (INFO): a request answered without a prompt, with the `operation` redacted as hook text is and clipped to 300 characters, since a command or address can carry a credential. `shell command failed` (WARN) redacts its `error` the same way.
- `config: skipped remote instructions <url>: <error>` and `config: skipped remote skill index <url>: <error>`: the address keeps its scheme, host and path, with its user name, password and credential-named query parameters as `<redacted>`, in the error too. The instruction prompt names a remote file the same way.

`program` is the file name of the command's first literal word after any `NAME=value` assignments and `env`/`exec`/`command`/`nohup` wrappers, never its arguments. When only the shell could resolve that word (command substitution, an expansion or unbalanced quotes), the shell's name is logged instead. Hook text is redacted before it is logged: Authorization, cookie and credential-named `key=value`, `key: value` and `--key value` pairs (token, secret, password, API key, private key, auth, credential, session), `Bearer`/`Basic` values, curl user flags, URL user info, JWTs, well-known token prefixes and long opaque strings become `[redacted]`. Control characters are removed, and text is clipped on a character boundary. Authorization, Cookie, Host and Origin header values are never logged. Every line written to the log file or stderr also shows each URL in it without its user name, password or credential-named query parameters, so a library that logs the address it could not reach (an MCP server's `?api_key=` in a connection error) leaves no key behind; on a coloured terminal the colour sequences around a URL are stepped over, so a URL right after one is still found.

## Data handling

Spans carry no prompts, model responses, tool arguments, tool output, file paths, HTTP headers, bearer tokens or OTLP authorization headers. Log lines are not exported with traces: their fields can hold provider error bodies, approval prompts, paths and client addresses, so they stay in the local log files. Only events written for traces (target `haarness::trace`: `child agent spawned`, and `retrying provider request` with the attempt and error category) become span events.

Processes the server starts for the agent, terminals, hooks, MCP servers and extensions do not inherit `OTEL_*` variables or the server's own credentials (`HAARNESS_SERVER_TOKEN`, `HAARNESS_SERVER_PASSWORD`, `HAARNESS_SERVER_USERNAME`). `OTEL_EXPORTER_OTLP_HEADERS` usually holds a collector key, and instrumented programs would otherwise export into your collector. Set a variable explicitly where a command needs it (a `shell.env` hook, or an MCP server's `env`).

The integration tests run the real executable against a mock collector and decode the exported protobuf. They check that a server request continues a W3C parent, with resource metadata, exporter headers, route and status. They check that one `haarness run` with tools, a failing tool and a subagent is a single trace with the tree above, error status, integer token usage, no polling spans and no prompt, path or collector key. And they check that unusable settings never fail a command:

```sh
cargo test -p haarness-cli --test otel_e2e
```
