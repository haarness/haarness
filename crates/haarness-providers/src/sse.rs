use async_stream::try_stream;
use futures::{Stream, StreamExt};
use haarness_core::{CoreError, CoreResult, StreamLimits, stream_stall};
use serde_json::Value;
use std::{pin::Pin, time::Duration};
use tokio_util::sync::CancellationToken;

pub type JsonStream = Pin<Box<dyn Stream<Item = CoreResult<Value>> + Send>>;

/// Keep the transport failure and endpoint useful in logs and persisted turn
/// errors without copying credentials from a configurable provider URL.
pub(crate) fn transport_error(mut error: reqwest::Error) -> CoreError {
    if let Some(url) = error.url_mut() {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
    }
    // reqwest's own text names the URL but not why it failed ("Connection
    // refused"), which decides whether a retry can help.
    let cause = std::iter::successors(std::error::Error::source(&error), |cause| cause.source())
        .last()
        .map(ToString::to_string);
    CoreError::Provider(match cause {
        Some(cause) => format!("{error}: {cause}"),
        None => error.to_string(),
    })
}

/// Whether `url` names this machine, where a provider's silence is its own
/// work (a local model reading a long prompt) rather than a lost connection.
pub(crate) fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Wait for `future` at most `limit`, failing as a silent provider.
async fn within<T>(limit: Option<Duration>, future: impl Future<Output = T>) -> CoreResult<T> {
    match limit {
        Some(limit) => tokio::time::timeout(limit, future)
            .await
            .map_err(|_| stream_stall(limit)),
        None => Ok(future.await),
    }
}

/// Send a streamed model request. Response headers that do not arrive within
/// the provider's idle limit fail the request as a silent provider; dropping
/// the returned future closes the connection.
pub(crate) async fn send(
    request: reqwest::RequestBuilder,
    limits: StreamLimits,
) -> CoreResult<reqwest::Response> {
    let (client, request) = request.build_split();
    let request = request.map_err(transport_error)?;
    let limit = limits.idle(is_loopback(request.url()));
    within(limit, client.execute(request))
        .await?
        .map_err(transport_error)
}

/// The response's server-sent events as JSON values. Any byte resets the idle
/// limit, so a provider's keep-alive pings and `:` comments count as activity;
/// a silence that outlasts it ends the stream with an error, and dropping the
/// stream closes the connection, so the provider stops generating.
pub fn json_stream(
    response: reqwest::Response,
    cancel: CancellationToken,
    limits: StreamLimits,
) -> JsonStream {
    let limit = limits.idle(is_loopback(response.url()));
    Box::pin(try_stream! {
        let response = if response.status().is_success() {
            response
        } else {
            let status = response.status();
            let detail = within(limit, response.text())
                .await?
                .ok()
                .and_then(|body| provider_error_detail(&body));
            let message = detail.map_or_else(
                || format!("HTTP {status}"),
                |detail| format!("HTTP {status}: {detail}"),
            );
            Err(CoreError::Provider(message))?;
            unreachable!();
        };
        let mut chunks = response.bytes_stream();
        let mut buffer = Vec::new();
        loop {
            let chunk = tokio::select! {
                () = cancel.cancelled() => Err(CoreError::Cancelled),
                chunk = within(limit, chunks.next()) => match chunk {
                    Err(stall) => Err(stall),
                    Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
                    Ok(Some(Err(error))) => Err(transport_error(error)),
                    Ok(None) => Ok(None),
                }
            };
            // A finished stream keeps what it held until it is dropped: close
            // the connection with the failure, not whenever the reader lets go.
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    drop(chunks);
                    Err(error)?;
                    unreachable!();
                }
            };
            let Some(chunk) = chunk else { break; };
            buffer.extend_from_slice(&chunk);
            while let Some(boundary) = find_boundary(&buffer) {
                let frame: Vec<u8> = buffer.drain(..boundary.0).collect();
                buffer.drain(..boundary.1);
                let mut data = String::new();
                for line in String::from_utf8_lossy(&frame).lines() {
                    if let Some(value) = line.strip_prefix("data:") {
                        if !data.is_empty() { data.push('\n'); }
                        data.push_str(value.trim_start());
                    }
                }
                if data.is_empty() || data == "[DONE]" { continue; }
                let value = serde_json::from_str(&data).map_err(|error| CoreError::Provider(format!("invalid SSE JSON: {error}")))?;
                yield value;
            }
        }
    })
}

/// The human-readable reason in an error body. Providers use several JSON
/// envelopes (`error.message`, `message`, FastAPI's `detail`), and gateways
/// sometimes answer in plain text; all of them beat a bare status line. HTML
/// error pages carry nothing actionable and are left out.
fn provider_error_detail(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let detail = match serde_json::from_str::<Value>(body) {
        Ok(parsed) => [
            "/error/message",
            "/error",
            "/message",
            "/detail",
            "/detail/message",
            "/detail/0/msg",
            "/errors/0/message",
        ]
        .iter()
        .find_map(|pointer| parsed.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .map(str::to_owned)?,
        Err(_) => {
            let lower = body.to_ascii_lowercase();
            if lower.starts_with("<!doctype") || lower.starts_with("<html") {
                return None;
            }
            body.split_whitespace().collect::<Vec<_>>().join(" ")
        }
    };
    // Keep machine-readable error codes without exposing unrelated response
    // fields. Managed-router admission failures are not upstream API quotas.
    let code = serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .filter(|code| {
                code.len() <= 80 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
            .map(str::to_owned)
    });
    let detail = code.map_or_else(|| detail.clone(), |code| format!("[{code}] {detail}"));
    let truncated = detail.chars().count() > 1_024;
    let mut detail: String = detail.chars().take(1_024).collect();
    if truncated {
        detail.push('…');
    }
    Some(detail)
}

fn find_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4))
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (index, 2))
        })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::StreamExt as _;
    use haarness_core::StreamLimits;
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpListener,
        sync::oneshot,
    };
    use tokio_util::sync::CancellationToken;

    use super::{find_boundary, is_loopback, json_stream, provider_error_detail, send};

    /// A provider that answers with server-sent `parts`, `pause` apart, and
    /// then stays silent with the connection open, or with no `head` never
    /// answers at all. The receiver resolves when the client closes the
    /// connection.
    async fn silent_provider(
        head: bool,
        parts: Vec<&'static str>,
        pause: Duration,
    ) -> (String, oneshot::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let (closed, observed) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            if head {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
                    .await
                    .unwrap();
                for part in parts {
                    socket.write_all(part.as_bytes()).await.unwrap();
                    tokio::time::sleep(pause).await;
                }
            }
            // Anything else the client sends is the rest of its request; the
            // end of input is the connection closing.
            while matches!(socket.read(&mut request).await, Ok(read) if read > 0) {}
            let _ = closed.send(());
        });
        (url, observed)
    }

    fn idle(limit: Duration) -> StreamLimits {
        StreamLimits {
            idle: Some(limit),
            total: None,
        }
    }

    #[tokio::test]
    async fn keep_alive_comments_count_and_a_silent_stream_closes_its_connection() {
        // Eight comments 300 ms apart outlast the 2 s limit together, but no
        // single silence does.
        let mut parts = vec![": keep-alive\n\n"; 8];
        parts.push("data: {\"n\":1}\n\n");
        let (url, closed) = silent_provider(true, parts, Duration::from_millis(300)).await;
        let limits = idle(Duration::from_secs(2));
        let response = send(reqwest::Client::new().post(&url).body("{}"), limits)
            .await
            .unwrap();
        let mut events = json_stream(response, CancellationToken::new(), limits);
        assert_eq!(events.next().await.unwrap().unwrap(), json!({"n": 1}));
        let error = events.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.to_string(),
            "provider error: the provider sent nothing for 2 seconds"
        );
        // The connection closes with the error, before anyone drops the stream.
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("the provider sees its connection close")
            .unwrap();
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn response_headers_that_never_come_fail_as_a_silent_provider() {
        let (url, closed) = silent_provider(false, Vec::new(), Duration::ZERO).await;
        let error = send(
            reqwest::Client::new().post(&url).body("{}"),
            idle(Duration::from_millis(300)),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "provider error: the provider sent nothing for 300 milliseconds"
        );
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("the provider sees its connection close")
            .unwrap();
    }

    #[test]
    fn only_this_machine_counts_as_loopback() {
        for local in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8000/v1",
            "http://127.1.2.3/v1",
            "http://[::1]:1234/v1",
        ] {
            assert!(is_loopback(&local.parse().unwrap()), "{local}");
        }
        for remote in [
            "https://api.anthropic.com/v1/messages",
            "http://192.168.1.20:11434/v1",
            "http://localhost.example.com/v1",
        ] {
            assert!(!is_loopback(&remote.parse().unwrap()), "{remote}");
        }
    }

    #[test]
    fn recognizes_lf_and_crlf_frames() {
        assert_eq!(find_boundary(b"data: {}\n\nmore"), Some((8, 2)));
        assert_eq!(find_boundary(b"data: {}\r\n\r\nmore"), Some((8, 4)));
    }

    #[test]
    fn extracts_provider_message_without_exposing_the_full_response() {
        assert_eq!(
            provider_error_detail(r#"{"error":{"message":"Invalid call_id"}}"#).as_deref(),
            Some("Invalid call_id")
        );
        assert_eq!(provider_error_detail(r#"{"error":{"code":"exposure_cap","message":"request reserve exceeded"},"secret":"must not appear"}"#).as_deref(), Some("[exposure_cap] request reserve exceeded"));
    }

    #[test]
    fn reads_every_common_error_envelope_and_plain_text() {
        assert_eq!(
            provider_error_detail(
                r#"{"detail":"The 'gpt-5.4-mini' model is not supported when using Codex with a ChatGPT account."}"#
            )
            .as_deref(),
            Some("The 'gpt-5.4-mini' model is not supported when using Codex with a ChatGPT account.")
        );
        assert_eq!(
            provider_error_detail(r#"{"error":"model_not_found"}"#).as_deref(),
            Some("model_not_found")
        );
        assert_eq!(
            provider_error_detail(
                r#"{"detail":[{"loc":["query","client_version"],"msg":"Field required"}]}"#
            )
            .as_deref(),
            Some("Field required")
        );
        assert_eq!(
            provider_error_detail("  upstream gateway\n timed out ").as_deref(),
            Some("upstream gateway timed out")
        );
        assert_eq!(provider_error_detail(""), None);
        assert_eq!(provider_error_detail(r#"{"error":{"code":400}}"#), None);
        assert_eq!(
            provider_error_detail("<!DOCTYPE html><html><body>Forbidden</body></html>"),
            None
        );
    }
}
