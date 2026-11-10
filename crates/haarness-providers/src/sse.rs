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
pub(crate) async fn within<T>(
    limit: Option<Duration>,
    future: impl Future<Output = T>,
) -> CoreResult<T> {
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
    framed_stream(response, cancel, limits, Framing::Events)
}

/// The response's lines as JSON values, one value a line (Ollama's own API
/// streams this way), with the same limits and errors as [`json_stream`].
pub(crate) fn json_lines_stream(
    response: reqwest::Response,
    cancel: CancellationToken,
    limits: StreamLimits,
) -> JsonStream {
    framed_stream(response, cancel, limits, Framing::Lines)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// Server-sent events: `data:` lines, a blank line between events.
    Events,
    /// One JSON value a line.
    Lines,
}

fn framed_stream(
    response: reqwest::Response,
    cancel: CancellationToken,
    limits: StreamLimits,
    framing: Framing,
) -> JsonStream {
    let limit = limits.idle(is_loopback(response.url()));
    Box::pin(try_stream! {
        let response = if response.status().is_success() {
            response
        } else {
            let status = response.status();
            let headers = response.headers().clone();
            let body = within(limit, response.text())
                .await?
                .unwrap_or_default();
            Err(status_error(status, &headers, &body))?;
            unreachable!();
        };
        let mut chunks = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut yielded = false;
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
            let Some(chunk) = chunk else {
                // A last line the server didn't end.
                if framing == Framing::Lines {
                    if let Some(value) = json_line(&buffer)? {
                        yield value;
                    }
                }
                break;
            };
            buffer.extend_from_slice(&chunk);
            if framing == Framing::Lines {
                while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=end).collect();
                    if let Some(value) = json_line(&line)? {
                        yield value;
                    }
                }
                continue;
            }
            while let Some(boundary) = find_boundary(&buffer) {
                let frame: Vec<u8> = buffer.drain(..boundary.0).collect();
                buffer.drain(..boundary.1);
                if let Some(value) = frame_data(&frame)? {
                    yielded = true;
                    yield value;
                }
            }
        }
        // What is left after the last blank line: a stream's final event
        // sent without one, or a whole reply from a server that does not
        // stream.
        let rest = String::from_utf8_lossy(&buffer).trim().to_owned();
        if rest.lines().any(|line| line.starts_with("data:")) {
            if let Some(value) = frame_data(rest.as_bytes())? {
                yield value;
            }
        } else if !yielded && !rest.is_empty() {
            yield whole_reply(&rest)?;
        }
    })
}

/// The JSON in one server-sent event, or `None` for an event with no data
/// (a comment, a keep-alive) and the closing `[DONE]`.
fn frame_data(frame: &[u8]) -> CoreResult<Option<Value>> {
    let mut data = String::new();
    for line in String::from_utf8_lossy(frame).lines() {
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|error| CoreError::Provider(format!("invalid SSE JSON: {error}")))
}

/// A successful answer that carried no events: a server that ignores
/// `stream` sends its reply whole, and the callers read a Chat Completions
/// reply sent whole as one event. An error sent with status 200 fails with
/// its own words; anything else is an address that serves no such API (Z.ai
/// answers one it does not serve with `{"code":500,"msg":"404 NOT_FOUND"}`).
fn whole_reply(body: &str) -> CoreResult<Value> {
    let value = serde_json::from_str::<Value>(body).ok();
    if let Some(value) = value
        .as_ref()
        .filter(|value| value.get("choices").is_some_and(Value::is_array))
    {
        return Ok(value.clone());
    }
    if let Some(detail) = value
        .as_ref()
        .filter(|value| value.get("error").is_some_and(|error| !error.is_null()))
        .and_then(|_| provider_error_detail(body))
    {
        return Err(CoreError::Provider(detail));
    }
    let said = value
        .as_ref()
        .and_then(|value| {
            ["/msg", "/message", "/detail"]
                .iter()
                .find_map(|pointer| value.pointer(pointer)?.as_str())
        })
        .map(str::trim)
        .filter(|said| !said.is_empty())
        .map(|said| {
            format!(
                " (it said \"{}\")",
                said.chars().take(200).collect::<String>()
            )
        })
        .unwrap_or_default();
    Err(CoreError::ProviderUnavailable(format!(
        "The provider answered without a reply{said}, so nothing at this address speaks its API. Check the provider's base URL against its API documentation."
    )))
}

/// The error for a refused streamed request: `HTTP <status>` with the
/// provider's own words, and the wait a rate limit names.
pub(crate) fn status_error(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) -> CoreError {
    let advised = advised_retry(status, headers);
    let detail = provider_error_detail(body);
    let mut message = detail.map_or_else(
        || format!("HTTP {status}"),
        |detail| format!("HTTP {status}: {detail}"),
    );
    if let Some(advised) = advised
        && !message.to_ascii_lowercase().contains("retry in")
    {
        message.push_str(&advised);
    }
    CoreError::Provider(message)
}

/// The wait a rate-limited response names in `Retry-After`, spelled the way
/// the runtime reads a provider's own advice (" Please retry in 7s."). A
/// limit's message alone ("Rate limit exceeded") says nothing about how long
/// it lasts, so without this a per-minute limit failed the turn at once. Only
/// a whole number of seconds is read; a date is left out.
pub(crate) fn advised_retry(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> Option<String> {
    if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let seconds: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(format!(" Please retry in {seconds}s."))
}

/// The human-readable reason in an error body. Providers use several JSON
/// envelopes (`error.message`, `message`, FastAPI's `detail`), and gateways
/// sometimes answer in plain text; all of them beat a bare status line. HTML
/// error pages carry nothing actionable and are left out.
pub(crate) fn provider_error_detail(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<Value>(body).ok().map(listed_error);
    let detail = match &parsed {
        // xAI answers some refusals with a bare JSON string.
        Some(Value::String(text)) if !text.trim().is_empty() => text.trim().to_owned(),
        Some(parsed) => match parsed.get("error").and_then(upstream_error) {
            Some(upstream) => upstream,
            None => [
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
        },
        None => {
            let lower = body.to_ascii_lowercase();
            if lower.starts_with("<!doctype") || lower.starts_with("<html") {
                return None;
            }
            body.split_whitespace().collect::<Vec<_>>().join(" ")
        }
    };
    let quotas = parsed.as_ref().map(quota_ids).unwrap_or_default();
    // Keep machine-readable error codes without exposing unrelated response
    // fields. Managed-router admission failures are not upstream API quotas.
    let code = parsed.and_then(|value| {
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
    for quota in quotas {
        detail.push_str("\n* Quota id: ");
        detail.push_str(&quota);
    }
    Some(detail)
}

/// The quotas a Google refusal names in its details
/// (`GenerateRequestsPerDayPerProjectPerModel-FreeTier`), which its message
/// leaves out: they say whether a limit is a minute's or a day's, and the
/// wait the message names ("Please retry in 11.6s") is the same either way.
fn quota_ids(body: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let details = body
        .pointer("/error/details")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for violation in details
        .filter_map(|detail| detail.get("violations")?.as_array())
        .flatten()
    {
        if let Some(id) = violation
            .get("quotaId")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 120
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            && !ids.iter().any(|known| known == id)
            && ids.len() < 4
        {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// The error a body holds in a list of one: Gemini's OpenAI-compatible API
/// answers `[{"error": {…}}]`, where its words and the wait a rate limit
/// names ("Please retry in 41s.") are.
fn listed_error(value: Value) -> Value {
    match value {
        Value::Array(mut items)
            if items.len() == 1 && items[0].get("error").is_some_and(|error| !error.is_null()) =>
        {
            items.swap_remove(0)
        }
        other => other,
    }
}

/// The upstream provider's own words in a gateway's error, which itself says
/// only "Provider returned error": OpenRouter carries them, and the provider's
/// name, in `metadata.raw` and `metadata.provider_name`. An upstream message
/// relayed as a raw server-sent event (`data: {"error":…}`) is unwrapped too.
pub(crate) fn upstream_error(error: &Value) -> Option<String> {
    let words = |text: &str| -> Option<String> {
        let text = text.trim();
        let json = text.strip_prefix("data:").unwrap_or(text).trim();
        let inner = serde_json::from_str::<Value>(json).ok().and_then(|value| {
            ["/error/message", "/message", "/error"]
                .iter()
                .find_map(|pointer| value.pointer(pointer)?.as_str().map(str::to_owned))
        });
        Some(inner.unwrap_or_else(|| text.to_owned()))
            .filter(|text| !text.is_empty() && !text.starts_with('{'))
    };
    let metadata = error.get("metadata");
    let detail = metadata
        .and_then(|metadata| metadata.get("raw")?.as_str())
        .and_then(words)
        .or_else(|| {
            let message = error.get("message")?.as_str()?;
            message
                .trim_start()
                .starts_with("data:")
                .then(|| words(message))
                .flatten()
        })?;
    Some(
        match metadata.and_then(|metadata| metadata.get("provider_name")?.as_str()) {
            Some(provider) => format!("{provider}: {detail}"),
            None => detail,
        },
    )
}

/// One line of a JSON-lines stream; `None` for a blank one.
fn json_line(line: &[u8]) -> CoreResult<Option<Value>> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(line)
        .map(Some)
        .map_err(|error| CoreError::Provider(format!("invalid JSON line: {error}")))
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
    #[test]
    fn a_rate_limit_passes_on_its_retry_after() {
        use reqwest::{
            StatusCode,
            header::{HeaderMap, HeaderValue, RETRY_AFTER},
        };
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        assert_eq!(
            advised_retry(StatusCode::TOO_MANY_REQUESTS, &headers).as_deref(),
            Some(" Please retry in 7s.")
        );
        // Only a rate limit, and only whole seconds.
        assert!(advised_retry(StatusCode::SERVICE_UNAVAILABLE, &headers).is_none());
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert!(advised_retry(StatusCode::TOO_MANY_REQUESTS, &headers).is_none());
        assert!(advised_retry(StatusCode::TOO_MANY_REQUESTS, &HeaderMap::new()).is_none());
    }

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

    use super::{
        advised_retry, find_boundary, is_loopback, json_stream, provider_error_detail, send,
    };

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

    /// A server that answers 200 with `body` and closes the connection.
    async fn answering(content_type: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        });
        url
    }

    async fn events_of(url: &str) -> Vec<Result<serde_json::Value, haarness_core::CoreError>> {
        let limits = idle(Duration::from_secs(5));
        let response = send(reqwest::Client::new().post(url).body("{}"), limits)
            .await
            .unwrap();
        json_stream(response, CancellationToken::new(), limits)
            .collect()
            .await
    }

    #[tokio::test]
    async fn a_last_event_without_a_blank_line_still_arrives() {
        let url = answering("text/event-stream", "data: {\"n\":1}\n\ndata: {\"n\":2}").await;
        let events: Vec<_> = events_of(&url)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(events, [json!({"n": 1}), json!({"n": 2})]);
    }

    #[tokio::test]
    async fn a_reply_sent_whole_is_one_event_and_an_error_body_says_so() {
        let url = answering(
            "application/json",
            r#"{"choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
        )
        .await;
        let events: Vec<_> = events_of(&url)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(events[0]["choices"][0]["message"]["content"], "hi");
        // Z.ai answers an address it does not serve this way, with 200.
        let url = answering(
            "application/json",
            r#"{"code":500,"msg":"404 NOT_FOUND","success":false}"#,
        )
        .await;
        let events = events_of(&url).await;
        let error = events.last().unwrap().as_ref().unwrap_err();
        assert!(
            matches!(error, haarness_core::CoreError::ProviderUnavailable(_)),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "The provider answered without a reply (it said \"404 NOT_FOUND\"), so nothing at this address speaks its API. Check the provider's base URL against its API documentation."
        );
        // An error sent with 200 is the provider's own.
        let url = answering(
            "application/json",
            r#"{"error":{"message":"Insufficient balance"}}"#,
        )
        .await;
        let events = events_of(&url).await;
        assert_eq!(
            events.last().unwrap().as_ref().unwrap_err().to_string(),
            "provider error: Insufficient balance"
        );
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

    #[test]
    fn an_error_listed_alone_is_read_as_the_error() {
        // Gemini's OpenAI-compatible API, as it answered a replayed call
        // without its signature and a rate limit on its free tier.
        let refused = "[{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Function call is missing a thought_signature in functionCall parts.\",\n    \"status\": \"INVALID_ARGUMENT\"\n  }\n}\n]\n";
        assert_eq!(
            provider_error_detail(refused).as_deref(),
            Some("Function call is missing a thought_signature in functionCall parts.")
        );
        let limited = r#"[{"error":{"code":429,"message":"You exceeded your current quota. Please retry in 41.2s.","status":"RESOURCE_EXHAUSTED"}}]"#;
        // The free tier's refusal of a model it leaves out keeps the words
        // the runtime reads ("free_tier", "limit: 0").
        let left_out = r#"[{"error":{"code":429,"message":"You exceeded your current quota, please check your plan and billing details.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 0, model: gemini-4-argon\nPlease retry in 12s.","status":"RESOURCE_EXHAUSTED"}}]"#;
        assert_eq!(
            provider_error_detail(left_out).as_deref(),
            Some(
                "You exceeded your current quota, please check your plan and billing details.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 0, model: gemini-4-argon\nPlease retry in 12s."
            )
        );
        let error = super::status_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            &reqwest::header::HeaderMap::new(),
            limited,
        );
        assert_eq!(
            error.to_string(),
            "provider error: HTTP 429 Too Many Requests: You exceeded your current quota. Please retry in 41.2s."
        );
        // A list of anything else says nothing readable, as before.
        assert_eq!(
            provider_error_detail(r#"[{"message":"one"},{"message":"two"}]"#),
            None
        );
    }

    /// Google names a day's quota used up with the same short wait as a
    /// minute's ("Please retry in 11.6s"); only the quota's id in the
    /// details says it is a day's, so the words keep it.
    #[test]
    fn a_google_refusal_keeps_the_ids_of_the_quotas_it_names() {
        let daily = r#"{"error":{"code":429,"message":"You exceeded your current quota, please check your plan and billing details.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 20, model: gemini-3.8-flash\nPlease retry in 11.615098148s.","status":"RESOURCE_EXHAUSTED","details":[
            {"@type":"type.googleapis.com/google.rpc.Help","links":[{"description":"Learn more about Gemini API quotas","url":"https://ai.google.dev/gemini-api/docs/rate-limits"}]},
            {"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[
                {"quotaMetric":"generativelanguage.googleapis.com/generate_content_free_tier_requests","quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier","quotaDimensions":{"location":"global","model":"gemini-3.8-flash"},"quotaValue":"20"},
                {"quotaMetric":"generativelanguage.googleapis.com/generate_content_free_tier_requests","quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"},
                {"quotaId":"not an id {\"x\"}"}
            ]},
            {"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"11s"}]}}"#;
        assert_eq!(
            provider_error_detail(daily).as_deref(),
            Some(
                "You exceeded your current quota, please check your plan and billing details.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 20, model: gemini-3.8-flash\nPlease retry in 11.615098148s.\n* Quota id: GenerateRequestsPerDayPerProjectPerModel-FreeTier"
            )
        );
        // Through its OpenAI-compatible API too, in a list of one.
        let listed = format!("[{daily}]");
        assert!(
            provider_error_detail(&listed)
                .unwrap()
                .ends_with("\n* Quota id: GenerateRequestsPerDayPerProjectPerModel-FreeTier")
        );
    }

    #[test]
    fn a_bare_json_string_is_the_reason() {
        // xAI's answer to its multi-agent model on chat completions.
        assert_eq!(
            provider_error_detail("\"Multi Agent requests are not allowed on chat completions\"")
                .as_deref(),
            Some("Multi Agent requests are not allowed on chat completions")
        );
        assert_eq!(provider_error_detail("\"\""), None);
    }

    #[test]
    fn a_gateway_error_names_the_upstream_provider_and_its_words() {
        // OpenRouter's answer when Google refused a request, as it arrived.
        let body = json!({"error":{"message":"Provider returned error","code":400,"metadata":{
            "raw":"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Request contains an invalid argument.\",\n    \"status\": \"INVALID_ARGUMENT\"\n  }\n}\n",
            "provider_name":"Google AI Studio","is_byok":false}},"user_id":"user_1"});
        assert_eq!(
            provider_error_detail(&body.to_string()).as_deref(),
            Some("Google AI Studio: Request contains an invalid argument.")
        );
        // An upstream timeout relayed inside the stream as a raw event.
        let relayed = json!({"code":502,"message":"data: {\"error\":{\"code\":\"TimeoutError\",\"message\":\"timed out during opening handshake\"}}\n\n","metadata":{"error_type":"provider_unavailable"}});
        assert_eq!(
            super::upstream_error(&relayed).as_deref(),
            Some("timed out during opening handshake")
        );
        assert_eq!(
            super::upstream_error(&json!({"message":"Rate limited","code":429})),
            None
        );
    }
}
