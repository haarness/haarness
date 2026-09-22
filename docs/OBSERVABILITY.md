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

## Data handling

The instrumentation does not add prompts, model responses, tool arguments, tool output, HTTP headers, bearer tokens, or OTLP authorization headers as span attributes. Existing structured log events can become span events and may contain the same operational paths and error messages already admitted to Haarness logs. Protect collector access and retention accordingly.

The integration test launches the real `haarness serve` executable, sends a request with a fixed W3C parent, shuts the server down through its authenticated API, decodes the exported protobuf in a local mock collector, and verifies resource metadata, batching/flush, exporter headers, HTTP status, and trace/parent IDs:

```sh
cargo test -p haarness-cli --test otel_e2e
```
