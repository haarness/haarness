//! Opt-in OpenTelemetry trace export (docs/OBSERVABILITY.md).
//!
//! Configuration follows the standard OTLP exporter and SDK environment
//! variables. A value Haarness cannot use is reported and ignored, never fatal:
//! a shell that sets `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` for another tool must
//! still run every `haarness` command.

use std::sync::{Mutex, OnceLock};

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::propagation::{TextMapCompositePropagator, TextMapPropagator};
use opentelemetry_otlp::{Protocol, WithExportConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};

/// Events with this target become span events in exported traces. Every other
/// log line stays in the local log files: log fields carry provider error
/// bodies, approval prompts, paths and client addresses that must not leave
/// the machine with a trace.
pub(crate) const TRACE_EVENT_TARGET: &str = haarness_core::TRACE_EVENT_TARGET;

/// What the process does, recorded as `haarness.process.role` so CLI, TUI and
/// server spans can be told apart in one trace.
#[derive(Clone, Copy)]
pub(crate) enum ProcessRole {
    Cli,
    Tui,
    Server,
}

impl ProcessRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Tui => "tui",
            Self::Server => "server",
        }
    }
}

/// The tracer provider, or why there is none, plus notices for the user.
pub(crate) struct Tracing {
    pub(crate) provider: Option<SdkTracerProvider>,
    /// Problems with the configuration that were ignored.
    pub(crate) notices: Vec<String>,
}

/// The configured OTLP traces endpoint, as the exporter will use it, when trace
/// export is on. `None` when off, disabled or invalid.
pub(crate) fn configured_endpoint() -> Option<String> {
    if sdk_disabled() {
        return None;
    }
    let (_, endpoint) = endpoint_setting()?;
    valid_endpoint(&endpoint).then_some(endpoint)
}

pub(crate) fn tracer_provider(role: ProcessRole) -> Tracing {
    let mut notices = Vec::new();
    if sdk_disabled() {
        return Tracing {
            provider: None,
            notices,
        };
    }
    let Some((variable, endpoint)) = endpoint_setting() else {
        return Tracing {
            provider: None,
            notices,
        };
    };
    if !valid_endpoint(&endpoint) {
        notices.push(format!(
            "{variable} is not an http(s) URL ({}); traces are not exported",
            shorten(&endpoint)
        ));
        return Tracing {
            provider: None,
            notices,
        };
    }
    if let Some((variable, protocol)) = setting(&[
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
        "OTEL_EXPORTER_OTLP_PROTOCOL",
    ]) && !protocol.eq_ignore_ascii_case("http/protobuf")
    {
        notices.push(format!(
            "{variable}={} is not supported; exporting traces with http/protobuf",
            shorten(&protocol)
        ));
    }
    // The exporter reads endpoint, headers, timeout and compression from the
    // environment itself; anything it rejects turns export off, not the command.
    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .build()
    {
        Ok(exporter) => exporter,
        Err(error) => {
            notices.push(format!(
                "trace export is off: {}",
                shorten(&error.to_string())
            ));
            return Tracing {
                provider: None,
                notices,
            };
        }
    };
    let provider = SdkTracerProvider::builder()
        .with_sampler(sampler(&mut notices))
        .with_resource(resource(role))
        .with_batch_exporter(exporter)
        .build();
    global::set_text_map_propagator(propagator(&mut notices));
    global::set_tracer_provider(provider.clone());
    Tracing {
        provider: Some(provider),
        notices,
    }
}

/// Flushes queued spans; returns a notice when that did not fully work.
pub(crate) fn shutdown(provider: &SdkTracerProvider) -> Option<String> {
    match provider.shutdown() {
        Ok(()) | Err(OTelSdkError::AlreadyShutdown) => None,
        Err(OTelSdkError::Timeout(after)) => Some(format!(
            "trace export did not finish within {} s; the last spans may be missing",
            after.as_secs()
        )),
        Err(error) => Some(format!(
            "could not export the last traces: {}",
            shorten(&error.to_string())
        )),
    }
}

fn sdk_disabled() -> bool {
    std::env::var("OTEL_SDK_DISABLED").is_ok_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// The first non-empty of the given variables, with its name.
fn setting(names: &[&'static str]) -> Option<(&'static str, String)> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(|value| (*name, value))
    })
}

fn endpoint_setting() -> Option<(&'static str, String)> {
    setting(&[
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
    ])
}

fn valid_endpoint(endpoint: &str) -> bool {
    reqwest::Url::parse(endpoint).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| !host.is_empty())
    })
}

fn sampler(notices: &mut Vec<String>) -> Sampler {
    let configured = std::env::var("OTEL_TRACES_SAMPLER")
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mut ratio = || -> f64 {
        let Ok(raw) = std::env::var("OTEL_TRACES_SAMPLER_ARG") else {
            return 1.0;
        };
        match raw.trim().parse::<f64>() {
            Ok(value) if (0.0..=1.0).contains(&value) => value,
            _ => {
                notices.push(format!(
                    "OTEL_TRACES_SAMPLER_ARG={} is not a ratio between 0 and 1; sampling everything",
                    shorten(&raw)
                ));
                1.0
            }
        }
    };
    match configured.as_str() {
        "" | "parentbased_always_on" => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
        "always_on" => Sampler::AlwaysOn,
        "always_off" => Sampler::AlwaysOff,
        "traceidratio" => Sampler::TraceIdRatioBased(ratio()),
        "parentbased_always_off" => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        "parentbased_traceidratio" => {
            Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio())))
        }
        other => {
            notices.push(format!(
                "OTEL_TRACES_SAMPLER={} is not supported; using parentbased_always_on",
                shorten(other)
            ));
            Sampler::ParentBased(Box::new(Sampler::AlwaysOn))
        }
    }
}

/// `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES` are read by the SDK's
/// default detectors; `haarness` is only the fallback service name.
fn resource(role: ProcessRole) -> Resource {
    let named = std::env::var("OTEL_SERVICE_NAME").is_ok_and(|value| !value.trim().is_empty())
        || std::env::var("OTEL_RESOURCE_ATTRIBUTES").is_ok_and(|value| {
            value.split(',').any(|pair| {
                pair.split('=')
                    .next()
                    .is_some_and(|key| key.trim() == "service.name")
            })
        });
    let builder = Resource::builder();
    let builder = if named {
        builder
    } else {
        builder.with_service_name("haarness")
    };
    builder
        .with_attributes([
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("service.instance.id", uuid::Uuid::new_v4().to_string()),
            KeyValue::new("process.pid", i64::from(std::process::id())),
            KeyValue::new("process.executable.name", "haarness"),
            KeyValue::new("haarness.process.role", role.as_str()),
        ])
        .build()
}

/// `OTEL_PROPAGATORS`: `tracecontext` (default), `baggage` and `none`.
fn propagator(notices: &mut Vec<String>) -> TextMapCompositePropagator {
    let configured = std::env::var("OTEL_PROPAGATORS").unwrap_or_default();
    let mut propagators: Vec<Box<dyn TextMapPropagator + Send + Sync>> = Vec::new();
    let names: Vec<String> = configured
        .split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() {
        propagators.push(Box::new(TraceContextPropagator::new()));
    }
    for name in &names {
        match name.as_str() {
            "tracecontext" => propagators.push(Box::new(TraceContextPropagator::new())),
            "baggage" => propagators.push(Box::new(BaggagePropagator::new())),
            "none" => {}
            other => notices.push(format!(
                "OTEL_PROPAGATORS value {} is not supported; Haarness propagates tracecontext and baggage",
                shorten(other)
            )),
        }
    }
    if !names.is_empty() && propagators.is_empty() && !names.iter().any(|name| name == "none") {
        propagators.push(Box::new(TraceContextPropagator::new()));
    }
    TextMapCompositePropagator::new(propagators)
}

fn shorten(value: &str) -> String {
    const LIMIT: usize = 160;
    let value = value.trim();
    if value.chars().count() <= LIMIT {
        value.to_owned()
    } else {
        format!("{}…", value.chars().take(LIMIT).collect::<String>())
    }
}

/// The first trace export failure the SDK reported in this process.
fn export_failure() -> &'static Mutex<Option<String>> {
    static FAILURE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    FAILURE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn take_export_failure() -> Option<String> {
    export_failure()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Remembers the SDK's own export errors (it logs them through `tracing` with
/// its crate name as the target) so a short command can say why spans are
/// missing instead of dropping them silently.
pub(crate) struct ExportFailureWatch;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ExportFailureWatch {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let metadata = event.metadata();
        if *metadata.level() > tracing::Level::WARN
            || !metadata.target().starts_with("opentelemetry")
        {
            return;
        }
        let mut slot = export_failure()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_some() {
            return;
        }
        let mut fields = FieldText::default();
        event.record(&mut fields);
        *slot = Some(shorten(if fields.0.is_empty() {
            metadata.name()
        } else {
            &fields.0
        }));
    }
}

#[derive(Default)]
struct FieldText(String);

impl tracing::field::Visit for FieldText {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if matches!(field.name(), "error" | "message" | "reason") {
            use std::fmt::Write as _;
            if !self.0.is_empty() {
                self.0.push_str("; ");
            }
            let _ = write!(self.0, "{value:?}");
            self.0 = self.0.trim_matches('"').to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_must_be_http_urls_with_a_host() {
        assert!(valid_endpoint("http://127.0.0.1:4318"));
        assert!(valid_endpoint("https://otel.example.com/v1/traces"));
        for endpoint in [
            "not-a-url",
            "127.0.0.1:4318",
            "ftp://example.com",
            "http://",
            "http://exa mple.com",
        ] {
            assert!(!valid_endpoint(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn long_values_are_shortened() {
        assert_eq!(shorten("  grpc "), "grpc");
        assert_eq!(shorten(&"x".repeat(200)).chars().count(), 161);
    }
}
