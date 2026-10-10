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
        .get(format!("http://{server_address}/v1/info"))
        .bearer_auth("otel-e2e-token")
        .header(
            "traceparent",
            "00-11111111111111111111111111111111-2222222222222222-01",
        )
        .send()
        .await
        .expect("call instrumented info endpoint");
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
                && string_attribute(&resource.attributes, "service.instance.id").is_some()
                && string_attribute(&resource.attributes, "haarness.process.role").as_deref()
                    == Some("server")
        })
    }));
    let spans = resource_spans
        .iter()
        .flat_map(|resource| &resource.scope_spans)
        .flat_map(|scope| &scope.spans)
        .collect::<Vec<&Span>>();
    // A server lives for hours; its requests are traced, not the whole process.
    assert!(!spans.iter().any(|span| span.name.starts_with("haarness ")));
    let info_span = spans
        .iter()
        .find(|span| span.name == "GET /v1/info")
        .expect("export the info request's server span");
    assert_eq!(info_span.trace_id, TRACE_ID);
    assert_eq!(info_span.parent_span_id, PARENT_SPAN_ID);
    assert_eq!(info_span.kind, 2, "HTTP span must have SERVER kind");
    assert_eq!(
        string_attribute(&info_span.attributes, "http.route").as_deref(),
        Some("/v1/info")
    );
    assert!(
        attribute(&info_span.attributes, "url.path").is_none(),
        "raw paths carry ids and share tokens"
    );
    assert_eq!(
        int_attribute(&info_span.attributes, "http.response.status_code"),
        Some(200)
    );
}

/// An isolated `haarness` invocation: its own HOME and data dir, no OTEL
/// settings from the developer's shell, and the scripted echo provider.
fn isolated(directory: &Path, workspace: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
    command
        .arg("--workspace")
        .arg(workspace)
        .arg("--data-dir")
        .arg(directory.join("data"))
        .current_dir(workspace)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory.join("home"))
        .env("XDG_CONFIG_HOME", directory.join("xdg-config"))
        .env("XDG_DATA_HOME", directory.join("xdg-data"))
        .env("XDG_CACHE_HOME", directory.join("xdg-cache"))
        .env("HAARNESS_HOME", directory.join("haarness-home"))
        .env("HAARNESS_TEST_PROVIDER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn mock_collector() -> (SocketAddr, CollectorState, tokio::task::JoinHandle<()>) {
    let state = CollectorState::default();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock OTLP collector");
    let address = listener.local_addr().expect("collector address");
    let router = Router::new()
        .route("/v1/traces", post(collect_traces))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (address, state, task)
}

fn exported_spans(state: &CollectorState) -> Vec<Span> {
    state
        .exports
        .lock()
        .expect("collector state lock")
        .iter()
        .flat_map(|export| {
            ExportTraceServiceRequest::decode(export.body.as_slice())
                .expect("decode OTLP protobuf export")
                .resource_spans
        })
        .flat_map(|resource| resource.scope_spans)
        .flat_map(|scope| scope.spans)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_run_is_one_trace_with_errors_usage_and_no_content() {
    let (collector, state, collector_task) = mock_collector().await;
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(workspace.join("README.md"), "# readme\n").expect("write readme");
    let otel = [
        ("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{collector}")),
        ("OTEL_EXPORTER_OTLP_HEADERS", "x-collector-key=SENTINEL_KEY".to_owned()),
        ("OTEL_BSP_SCHEDULE_DELAY", "25".to_owned()),
    ];
    let prompt = r#"!multi [{"name":"read","arguments":{"path":"README.md"}},{"name":"read","arguments":{"path":"SENTINEL_MISSING.txt"}},{"name":"bash","arguments":{"command":"printenv > env.txt"}},{"name":"task","arguments":{"description":"child","prompt":"SENTINEL_CHILD","agent":"general"}}]"#;
    let output = timeout(
        Duration::from_secs(60),
        isolated(&directory.0, &workspace)
            .envs(otel.clone())
            .args(["--yolo", "run", "--provider", "echo", "--model", "echo-1", prompt])
            .output(),
    )
    .await
    .expect("haarness run finished within a minute")
    .expect("run haarness");
    // The run's daemon flushes its spans when it stops.
    let _ = timeout(
        Duration::from_secs(30),
        isolated(&directory.0, &workspace)
            .envs(otel)
            .args(["serve", "--stop"])
            .output(),
    )
    .await;
    collector_task.abort();
    assert!(
        output.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Commands the agent runs never see the collector key or the server's credentials.
    let environment = std::fs::read_to_string(workspace.join("env.txt")).expect("read env.txt");
    assert!(!environment.contains("OTEL_"), "{environment}");
    assert!(!environment.contains("HAARNESS_SERVER_"), "{environment}");

    let spans = exported_spans(&state);
    let named = |name: &'static str| spans.iter().filter(move |span| span.name == name);
    let command = named("haarness run").next().expect("command span");
    let in_run = |span: &&Span| span.trace_id == command.trace_id;
    let parent_of = |span: &Span| {
        spans
            .iter()
            .find(|candidate| candidate.span_id == span.parent_span_id)
            .map(|parent| parent.name.as_str())
    };
    let root_turn = named("invoke_agent build").next().expect("root agent turn");
    assert!(in_run(&root_turn), "the turn joins the command's trace");
    assert_eq!(parent_of(root_turn), Some("start_turn"));
    let request = spans
        .iter()
        .find(|span| span.name == "POST /v1/threads/{id}/turns")
        .expect("turn request span");
    assert_eq!(parent_of(request), Some("haarness.client.turn"));
    let tools: Vec<&Span> = named("execute_tool read").collect();
    assert_eq!(tools.len(), 2);
    assert!(tools.iter().all(|tool| in_run(tool) && parent_of(tool) == Some("invoke_agent build")));
    let failed = tools
        .iter()
        .find(|tool| tool.status.as_ref().is_some_and(|status| status.code == 2))
        .expect("the missing file's read is a failed span");
    assert_eq!(
        string_attribute(&failed.attributes, "error.type").as_deref(),
        Some("tool_error")
    );
    let child = named("invoke_agent general").next().expect("subagent turn");
    assert!(in_run(&child));
    assert_eq!(parent_of(child), Some("execute_tool task"));
    let chat = named("chat echo-1").next().expect("model request span");
    assert!(int_attribute(&chat.attributes, "gen_ai.usage.input_tokens").is_some());
    assert!(int_attribute(&chat.attributes, "gen_ai.usage.output_tokens").is_some());

    // Polling reads stay out of traces, and nothing the person wrote leaves with one.
    assert!(!spans.iter().any(|span| span.name == "GET /v1/turns/{id}"));
    let exported = format!("{spans:?}");
    for sentinel in ["SENTINEL_KEY", "SENTINEL_MISSING", "SENTINEL_CHILD", "README.md", "printenv"] {
        assert!(!exported.contains(sentinel), "{sentinel} was exported");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_haarness_cannot_use_never_fail_a_command() {
    let directory = TestDirectory::new();
    let workspace = directory.0.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    for (name, value) in [
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ("OTEL_TRACES_SAMPLER", "bogus"),
        ("OTEL_TRACES_SAMPLER_ARG", "2"),
        ("OTEL_PROPAGATORS", "b3"),
        ("OTEL_EXPORTER_OTLP_COMPRESSION", "zstd"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "not-a-url"),
    ] {
        let mut command = isolated(&directory.0, &workspace);
        if name != "OTEL_EXPORTER_OTLP_ENDPOINT" {
            command.env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9");
            command.env("OTEL_EXPORTER_OTLP_TIMEOUT", "200");
        }
        let output = command
            .env(name, value)
            .arg("agents")
            .output()
            .await
            .expect("run haarness agents");
        assert!(
            output.status.success(),
            "{name}={value} failed the command:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
