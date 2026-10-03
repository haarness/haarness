use async_stream::try_stream;
use futures::{Stream, StreamExt};
use haarness_core::{CoreError, CoreResult};
use serde_json::Value;
use std::pin::Pin;
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
    CoreError::Provider(error.to_string())
}

pub fn json_stream(response: reqwest::Response, cancel: CancellationToken) -> JsonStream {
    Box::pin(try_stream! {
        let response = if response.status().is_success() {
            response
        } else {
            let status = response.status();
            let detail = response
                .text()
                .await
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
                chunk = chunks.next() => match chunk {
                    Some(Ok(chunk)) => Ok(Some(chunk)),
                    Some(Err(error)) => Err(transport_error(error)),
                    None => Ok(None),
                }
            }?;
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
    use super::{find_boundary, provider_error_detail};

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
