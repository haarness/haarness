use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    routing::post,
};
use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value},
    trace::v1::Span,
};
use prost::Message as _;
use tokio::{net::TcpListener, process::Command, time::timeout};
use uuid::Uuid;

const TRACE_ID: [u8; 16] = [0x11; 16];
const PARENT_SPAN_ID: [u8; 8] = [0x22; 8];

#[derive(Clone, Default)]
struct CollectorState {
    exports: Arc<Mutex<Vec<CapturedExport>>>,
}

struct CapturedExport {
    headers: HeaderMap,
    body: Vec<u8>,
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("haarness-otel-e2e-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("create isolated test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn collect_traces(
    State(state): State<CollectorState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, [(axum::http::HeaderName, &'static str); 1]) {
    state
        .exports
        .lock()
        .expect("collector state lock")
        .push(CapturedExport {
            headers,
            body: body.to_vec(),
        });
    (StatusCode::OK, [(CONTENT_TYPE, "application/x-protobuf")])
}

async fn unused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    listener.local_addr().expect("ephemeral address")
}

async fn wait_until_listening(address: SocketAddr) {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Haarness did not listen on {address} within five seconds");
}

fn attribute<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| attribute.value.as_ref())
}

fn string_attribute(attributes: &[KeyValue], key: &str) -> Option<String> {
    match attribute(attributes, key)?.value.as_ref()? {
        any_value::Value::StringValue(value) => Some(value.clone()),
        _ => None,
    }
}

fn int_attribute(attributes: &[KeyValue], key: &str) -> Option<i64> {
    match attribute(attributes, key)?.value.as_ref()? {
        any_value::Value::IntValue(value) => Some(*value),
        _ => None,
    }
}

fn command_for_test(workspace: &Path, data: &Path, server: SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
    command
        .arg("--workspace")
        .arg(workspace)
        .arg("--data-dir")
        .arg(data)
        .arg("serve")
        .arg("--host")
        .arg(server.ip().to_string())
        .arg("--port")
        .arg(server.port().to_string())
        .arg("--token")
        .arg("otel-e2e-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for variable in [
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_HEADERS",
        "OTEL_EXPORTER_OTLP_PROTOCOL",
        "OTEL_RESOURCE_ATTRIBUTES",
        "OTEL_SDK_DISABLED",
        "OTEL_SERVICE_NAME",
        "OTEL_TRACES_SAMPLER",
        "OTEL_TRACES_SAMPLER_ARG",
    ] {
        command.env_remove(variable);
    }
    command
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_otlp_protobuf_and_continues_inbound_trace_context() {
    let collector_state = CollectorState::default();
    let collector_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock OTLP collector");
    let collector_address = collector_listener.local_addr().expect("collector address");
    let collector_task = tokio::spawn(
        axum::serve(
            collector_listener,
            Router::new()
                .route("/v1/traces", post(collect_traces))
                .with_state(collector_state.clone()),
        )
        .into_future(),
    );

    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    let data = directory.0.join("data");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let server_address = unused_address().await;
    let mut command = command_for_test(&workspace, &data, server_address);
    command
        .env(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://{collector_address}"),
        )
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .env("OTEL_EXPORTER_OTLP_HEADERS", "x-haarness-test=accepted")
        .env("OTEL_SERVICE_NAME", "haarness-e2e")
        .env("OTEL_BSP_SCHEDULE_DELAY", "25")
        .env("OTEL_TRACES_SAMPLER", "always_on")
        .env("RUST_LOG", "warn");
    let child = command.spawn().expect("launch Haarness server");
    wait_until_listening(server_address).await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{server_address}/healthz"))
        .header(
            "traceparent",
            "00-11111111111111111111111111111111-2222222222222222-01",
        )
        .send()
        .await
        .expect("call instrumented health endpoint");
    assert_eq!(response.status(), StatusCode::OK);
    let shutdown = client
        .post(format!("http://{server_address}/v1/shutdown"))
        .bearer_auth("otel-e2e-token")
        .send()
        .await
        .expect("request graceful shutdown");
    assert_eq!(shutdown.status(), StatusCode::ACCEPTED);

    let output = timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("Haarness did not shut down within 30 seconds")
        .expect("wait for Haarness process");
    assert!(
        output.status.success(),
        "Haarness failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    collector_task.abort();

    let exports = collector_state
        .exports
        .lock()
        .expect("collector state lock");
    assert!(
        !exports.is_empty(),
        "mock collector received no OTLP exports\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(exports.iter().all(|export| {
        export
            .headers
            .get("x-haarness-test")
            .is_some_and(|value| value == "accepted")
            && export
                .headers
                .get(CONTENT_TYPE)
                .is_some_and(|value| value == "application/x-protobuf")
    }));
    let decoded = exports
        .iter()
        .map(|export| {
            ExportTraceServiceRequest::decode(export.body.as_slice())
                .expect("decode OTLP protobuf export")
        })
        .collect::<Vec<_>>();
    let resource_spans = decoded
        .iter()
        .flat_map(|export| &export.resource_spans)
        .collect::<Vec<_>>();
    assert!(resource_spans.iter().any(|resource_spans| {
        resource_spans.resource.as_ref().is_some_and(|resource| {
            string_attribute(&resource.attributes, "service.name").as_deref()
                == Some("haarness-e2e")
                && string_attribute(&resource.attributes, "service.version").is_some()
        })
    }));
    let spans = resource_spans
        .iter()
        .flat_map(|resource| &resource.scope_spans)
        .flat_map(|scope| &scope.spans)
        .collect::<Vec<&Span>>();
    let command_span = spans
        .iter()
        .find(|span| span.name == "haarness.command")
        .expect("export haarness.command span");
    assert_eq!(
        string_attribute(&command_span.attributes, "haarness.command.name").as_deref(),
        Some("serve")
    );
    let health_span = spans
        .iter()
        .find(|span| {
            span.name == "http.request"
                && string_attribute(&span.attributes, "url.path").as_deref() == Some("/healthz")
        })
        .expect("export health HTTP server span");
    assert_eq!(health_span.trace_id, TRACE_ID);
    assert_eq!(health_span.parent_span_id, PARENT_SPAN_ID);
    assert_eq!(health_span.kind, 2, "HTTP span must have SERVER kind");
    assert_eq!(
        int_attribute(&health_span.attributes, "http.response.status_code"),
        Some(200)
    );
}
