//! Checking an API key before it is stored: one cheap authenticated request,
//! normally a model listing, whose answer says whether the provider knows the
//! key. Only a clear refusal rejects a key; anything uncertain leaves it
//! unchecked so a flaky network never blocks connecting.

use haarness_core::KeyCheck;
use serde_json::Value;
use std::time::Duration;

/// How long a check waits before the key is saved unchecked.
const TIMEOUT: Duration = Duration::from_secs(8);

/// Send `request` (already carrying the key) to the provider called `name`.
pub(crate) async fn check(name: &str, request: reqwest::RequestBuilder) -> KeyCheck {
    let response = match request.timeout(TIMEOUT).send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return KeyCheck::Unchecked(format!("{name} did not answer in time"));
        }
        Err(_) => return KeyCheck::Unchecked(format!("Haarness could not reach {name}")),
    };
    let status = response.status();
    if status.is_success() {
        return KeyCheck::Accepted;
    }
    let reason = response.text().await.ok().and_then(|body| reason(&body));
    classify(name, status.as_u16(), reason)
}

fn classify(name: &str, status: u16, reason: Option<String>) -> KeyCheck {
    let lower = reason.as_deref().unwrap_or_default().to_ascii_lowercase();
    match status {
        // A restricted key that may not list models still authenticated.
        401 | 403 if lower.contains("scope") || lower.contains("insufficient permission") => {
            KeyCheck::Accepted
        }
        401 | 403 => KeyCheck::Rejected(reason),
        // Google answers an unknown key with 400 "API key not valid".
        400 if lower.contains("api key") => KeyCheck::Rejected(reason),
        // No listing route to ask: nothing is known about the key.
        404 | 405 => KeyCheck::Unsupported,
        _ => KeyCheck::Unchecked(format!("{name} answered HTTP {status}")),
    }
}

/// The provider's own words from an error body, short enough for one line.
fn reason(body: &str) -> Option<String> {
    let body = body.trim();
    let text = match serde_json::from_str::<Value>(body) {
        Ok(value) => ["/error/message", "/message", "/error", "/detail"]
            .iter()
            .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))?
            .to_owned(),
        Err(_) if body.starts_with('<') => return None,
        Err(_) => body.to_owned(),
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.trim_end_matches('.');
    if text.is_empty() {
        return None;
    }
    let mut short: String = text.chars().take(200).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    Some(short)
}

#[cfg(test)]
mod tests {
    use super::{KeyCheck, check, classify, reason};

    #[test]
    fn only_a_clear_refusal_rejects_a_key() {
        assert_eq!(
            classify("Anthropic", 401, Some("invalid x-api-key".into())),
            KeyCheck::Rejected(Some("invalid x-api-key".into()))
        );
        assert_eq!(
            classify("Google Gemini", 400, Some("API key not valid".into())),
            KeyCheck::Rejected(Some("API key not valid".into()))
        );
        assert_eq!(
            classify(
                "OpenAI",
                401,
                Some("You have insufficient permissions for this operation. Missing scopes: api.model.read".into())
            ),
            KeyCheck::Accepted
        );
        assert_eq!(classify("Acme", 404, None), KeyCheck::Unsupported);
        assert_eq!(
            classify("Acme", 503, None),
            KeyCheck::Unchecked("Acme answered HTTP 503".into())
        );
        assert_eq!(
            classify("Acme", 400, Some("bad model".into())),
            KeyCheck::Unchecked("Acme answered HTTP 400".into())
        );
    }

    #[test]
    fn reasons_are_the_providers_words_on_one_line() {
        assert_eq!(
            reason(r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#)
                .as_deref(),
            Some("invalid x-api-key")
        );
        assert_eq!(reason("Unauthorized.\n").as_deref(), Some("Unauthorized"));
        assert_eq!(reason("<html><body>401</body></html>"), None);
        assert_eq!(reason(""), None);
    }

    #[tokio::test]
    async fn an_unreachable_provider_leaves_the_key_unchecked() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let request = reqwest::Client::new().get(format!("http://127.0.0.1:{port}/models"));
        assert_eq!(
            check("Acme", request).await,
            KeyCheck::Unchecked("Haarness could not reach Acme".into())
        );
    }
}
