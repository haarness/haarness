//! Checking an API key before it is stored: one cheap authenticated request,
//! normally a model listing, whose answer says whether the provider knows the
//! key. Only a clear refusal from a provider's own API rejects a key; anything
//! uncertain leaves it unchecked so a flaky network or a gateway that guards
//! the listing route never blocks connecting.

use haarness_core::KeyCheck;
use serde_json::Value;
use std::time::Duration;

/// How long a check waits before the key is saved unchecked.
const TIMEOUT: Duration = Duration::from_secs(8);

/// Hosts of the native providers' own APIs. Compatible presets add theirs
/// from [`crate::COMPAT_PRESETS`].
const OFFICIAL_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "generativelanguage.googleapis.com",
];

/// Send `request` (already carrying the key) to the provider called `name`.
pub(crate) async fn check(name: &str, request: reqwest::RequestBuilder) -> KeyCheck {
    let (client, request) = request.timeout(TIMEOUT).build_split();
    let Ok(request) = request else {
        return KeyCheck::Unchecked(format!("Haarness could not reach {name}"));
    };
    let official = official_api(request.url());
    send(name, client.execute(request), official).await
}

/// Whether `url` is a provider's own public API, where a refused listing
/// means a refused key. Gateways and proxies in front of a provider may
/// refuse routes they do not serve (AWS API Gateway answers 403 "Missing
/// Authentication Token"), so there a refusal leaves the key unchecked.
fn official_api(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    url.scheme() == "https"
        && (OFFICIAL_HOSTS.contains(&host)
            || (host.starts_with("bedrock.") && host.ends_with(".amazonaws.com"))
            || crate::COMPAT_PRESETS.iter().any(|preset| {
                reqwest::Url::parse(preset.base_url).is_ok_and(|base| base.host_str() == Some(host))
            }))
}

async fn send(
    name: &str,
    response: impl std::future::Future<Output = reqwest::Result<reqwest::Response>>,
    official: bool,
) -> KeyCheck {
    let response = match response.await {
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
    classify(name, status.as_u16(), reason, official)
}

fn classify(name: &str, status: u16, reason: Option<String>, official: bool) -> KeyCheck {
    let lower = reason.as_deref().unwrap_or_default().to_ascii_lowercase();
    let refused = match status {
        // A restricted key that may not list models still authenticated.
        // AWS: a key whose policy leaves out the listing is still a key.
        401 | 403
            if lower.contains("scope")
                || lower.contains("insufficient permission")
                || lower.contains("not authorized to perform") =>
        {
            return KeyCheck::Accepted;
        }
        401 | 403 => true,
        // Google answers an unknown key with 400 "API key not valid".
        400 => lower.contains("api key"),
        // No listing route to ask: nothing is known about the key.
        404 | 405 => return KeyCheck::Unsupported,
        _ => false,
    };
    match (refused, official) {
        (true, true) => KeyCheck::Rejected(reason),
        // A gateway's refusal may be about the route, not the key: say what
        // it answered and keep the key.
        (true, false) => KeyCheck::Unchecked(format!(
            "{name} answered HTTP {status}{}",
            reason
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        )),
        (false, _) => KeyCheck::Unchecked(format!("{name} answered HTTP {status}")),
    }
}

/// The provider's own words from an error body, short enough for one line.
fn reason(body: &str) -> Option<String> {
    let body = body.trim();
    let text = match serde_json::from_str::<Value>(body) {
        Ok(value) => [
            "/error/message",
            "/message",
            "/Message",
            "/error",
            "/detail",
        ]
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
    use super::{KeyCheck, check, classify, official_api, reason, send};

    #[test]
    fn only_a_clear_refusal_from_a_providers_own_api_rejects_a_key() {
        // Bedrock: an unknown API key, and a known one its policy keeps from listing.
        assert!(official_api(
            &reqwest::Url::parse("https://bedrock.us-east-1.amazonaws.com/foundation-models")
                .unwrap()
        ));
        assert_eq!(
            reason(
                r#"{"Message":"Authentication failed: Please make sure your API Key is valid."}"#
            ),
            Some("Authentication failed: Please make sure your API Key is valid".into())
        );
        assert_eq!(
            classify(
                "Amazon Bedrock",
                403,
                Some("Authentication failed: Please make sure your API Key is valid".into()),
                true
            ),
            KeyCheck::Rejected(Some(
                "Authentication failed: Please make sure your API Key is valid".into()
            ))
        );
        assert_eq!(
            classify(
                "Amazon Bedrock",
                403,
                Some("User: arn:aws:iam::123456789012:user/BedrockAPIKey-x is not authorized to perform: bedrock:ListFoundationModels".into()),
                true
            ),
            KeyCheck::Accepted
        );
        assert_eq!(
            classify("Anthropic", 401, Some("invalid x-api-key".into()), true),
            KeyCheck::Rejected(Some("invalid x-api-key".into()))
        );
        assert_eq!(
            classify("Google Gemini", 400, Some("API key not valid".into()), true),
            KeyCheck::Rejected(Some("API key not valid".into()))
        );
        assert_eq!(
            classify(
                "OpenAI",
                401,
                Some("You have insufficient permissions for this operation. Missing scopes: api.model.read".into()),
                true
            ),
            KeyCheck::Accepted
        );
        assert_eq!(classify("Acme", 404, None, true), KeyCheck::Unsupported);
        assert_eq!(
            classify("Acme", 503, None, true),
            KeyCheck::Unchecked("Acme answered HTTP 503".into())
        );
        assert_eq!(
            classify("Acme", 400, Some("bad model".into()), true),
            KeyCheck::Unchecked("Acme answered HTTP 400".into())
        );
    }

    #[test]
    fn a_gateways_refusal_leaves_the_key_unchecked() {
        assert_eq!(
            classify(
                "Acme",
                403,
                Some("Missing Authentication Token".into()),
                false
            ),
            KeyCheck::Unchecked("Acme answered HTTP 403 (Missing Authentication Token)".into())
        );
        assert_eq!(
            classify("Acme", 401, None, false),
            KeyCheck::Unchecked("Acme answered HTTP 401".into())
        );
        assert_eq!(
            classify("Acme", 400, Some("API key not valid".into()), false),
            KeyCheck::Unchecked("Acme answered HTTP 400 (API key not valid)".into())
        );
        assert_eq!(classify("Acme", 405, None, false), KeyCheck::Unsupported);
    }

    #[test]
    fn official_apis_are_the_native_and_preset_hosts_over_https() {
        let official = |url: &str| official_api(&reqwest::Url::parse(url).unwrap());
        assert!(official("https://api.anthropic.com/v1/models?limit=1"));
        assert!(official("https://api.openai.com/v1/models"));
        assert!(official(
            "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1"
        ));
        assert!(official("https://api.groq.com/openai/v1/models"));
        assert!(official("https://api.together.ai/v1/models"));
        assert!(!official("http://api.openai.com/v1/models"));
        assert!(!official("https://gateway.example.com/v1/models"));
        assert!(!official(
            "https://abc123.execute-api.us-east-1.amazonaws.com/prod/models"
        ));
        assert!(!official("http://127.0.0.1:4000/v1/models"));
    }

    #[tokio::test]
    async fn a_listing_refused_by_a_gateway_keeps_the_key_and_by_a_provider_rejects_it() {
        use axum::{Json, Router, http::StatusCode, routing::get};
        let router = Router::new().route(
            "/models",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"message": "Missing Authentication Token"})),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/models");
        assert_eq!(
            check("Acme", client.get(&url).bearer_auth("sk-real")).await,
            KeyCheck::Unchecked("Acme answered HTTP 403 (Missing Authentication Token)".into())
        );
        assert_eq!(
            send("Acme", client.get(&url).send(), true).await,
            KeyCheck::Rejected(Some("Missing Authentication Token".into()))
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
