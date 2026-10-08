# Observability

Haarness keeps its existing structured daily logs and can additionally export traces through native OpenTelemetry OTLP. Export is disabled unless an endpoint is configured, so a normal local invocation does not contact a collector.

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

Supported sampler names are `always_on`, `always_off`, `traceidratio`, `parentbased_always_on`, `parentbased_always_off`, and `parentbased_traceidratio`. The default is `parentbased_always_on`. Set `OTEL_SDK_DISABLED=true` to disable export even when an endpoint remains in the environment.

Haarness deliberately rejects configured `grpc` or `http/json` protocols because this binary ships only the `http/protobuf` transport. It fails during startup rather than silently sending with a different protocol.

`HAARNESS_LOG` and `RUST_LOG` continue to control file/stderr logs. They do not suppress OTLP spans. Use `HAARNESS_OTEL_FILTER` for an independent `tracing-subscriber` filter; its default records `info` and above for Haarness crates and `warn` and above elsewhere.

## Span contract

The exported resource includes `service.name`, `service.version`, the OpenTelemetry SDK attributes, and configured `OTEL_RESOURCE_ATTRIBUTES`. The primary spans are:

- `haarness.command`: one process-level command, including the command name and workspace ID.
- `http.request`: server requests with W3C `traceparent`/`tracestate` extraction, method, path without query parameters, response status, and server span kind.
- `agent.turn`: a durable agent turn with thread/turn IDs, agent, provider/model, and child depth.
- `provider.request`: provider stream latency with provider/model and agent identifiers.
- `tool.call`: tool latency with tool name/call ID and agent identifiers.

Existing structured `tracing` events emitted inside those spans are available as span events. The batch processor is flushed during ordinary process shutdown, including graceful `serve` shutdown.

## Diagnostic log lines

These lines are designed to be safe to keep in daemon logs and span events:

- `hook command failed` (WARN): a hook exited non-zero (other than a clean exit-2 block) or was killed, including `async` hooks. Fields: `event`, `tool` when the event has one, `program`, `code` or `signal`, `asynchronous`, and the start of `stderr` (up to 300 characters). Such a failure never blocks the action.
- `hook blocked` (INFO): exactly once per block from `PreToolUse`, `PermissionRequest` or `UserPromptSubmit`, whether by exit code 2, a JSON `block`/`continue: false`, or a permission `deny`. Fields: `event`, `tool`, `program`, `code` and the first line of the `reason` (up to 200 characters). A block from a command that also failed logs both lines. Observational events cannot block, so their block answers log only at DEBUG.
- `hook timed out`, `hook failed to spawn` and `hook failed` (WARN) carry the same `event`, `tool` and `program` fields.
- `request rejected` (WARN): the API refused a request with 401 or 403. Fields: `method`, `path` without the query string, `reason` (`missing`, `wrong_scheme`, `bad_credential` or `forbidden` for a host/origin check), the Authorization `scheme` name only (`Bearer`, `Basic`, … or `unrecognized`), `remote` address and a clipped `user_agent`. At most one line per path prefix (for example `/v1/threads`) and reason is written each minute; the next line reports how many were `suppressed`.
- `haarness starting` (INFO): every command's first line. Fields: `version`, `commit` (the git commit the binary was built from, `<sha>` or `<sha>-dirty`, or `unknown` for a build outside this repository's own git checkout), and `workspace`. `-dirty` reflects the tree when the build script last ran, which happens when HEAD moves.
- `hot reload source scan failed` (WARN): a watched configuration source could not be read, with the path in `error`. The watcher keeps polling; repeats of the same error log at DEBUG, a different error warns again, and `hot reload source scan recovered` (INFO) follows once the sources read again.
- `skipping provider-incompatible tool schema for Claude` (WARN): a tool was left out of Claude requests because Claude rejects its root input schema. Fields: `tool`, `issues`. Once per tool per process.
- `truncated MCP tool description` (DEBUG): fields `tool`, `original_chars` and `limit`.

`program` is the file name of the command's first literal word after any `NAME=value` assignments and `env`/`exec`/`command`/`nohup` wrappers, never its arguments. When only the shell could resolve that word (command substitution, an expansion or unbalanced quotes), the shell's name is logged instead. Hook text is redacted before it is logged: Authorization, cookie and credential-named `key=value`, `key: value` and `--key value` pairs (token, secret, password, API key, private key, auth, credential, session), `Bearer`/`Basic` values, curl user flags, URL user info, JWTs, well-known token prefixes and long opaque strings become `[redacted]`. Control characters are removed, and text is clipped on a character boundary. Authorization, Cookie, Host and Origin header values are never logged.

## Data handling

The instrumentation does not add prompts, model responses, tool arguments, tool output, HTTP headers, bearer tokens, or OTLP authorization headers as span attributes. Existing structured log events can become span events and may contain the same operational paths and error messages already admitted to Haarness logs. Protect collector access and retention accordingly.

The integration test launches the real `haarness serve` executable, sends a request with a fixed W3C parent, shuts the server down through its authenticated API, decodes the exported protobuf in a local mock collector, and verifies resource metadata, batching/flush, exporter headers, HTTP status, and trace/parent IDs:

```sh
cargo test -p haarness-cli --test otel_e2e
```
