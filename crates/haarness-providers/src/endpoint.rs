//! Custom endpoints as people set them up: the base URL normalised the way
//! they expect, and a check of the address before it is saved, so a
//! provider that cannot work is never saved without saying why.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::CompatibleProtocol;

/// How long the set-up check waits for each answer.
const TIMEOUT: Duration = Duration::from_secs(8);

/// Paths that name one endpoint of an OpenAI-style API rather than its base,
/// as people paste them from a provider's curl example.
const OPENAI_ENDPOINTS: [&str; 4] = ["/chat/completions", "/completions", "/responses", "/models"];

/// Whether the URL's last path segment is an API version (`v1`, `v4`,
/// `v1beta`), after which an API's endpoints follow directly.
#[must_use]
pub fn ends_with_version(url: &str) -> bool {
    let url = url.trim_end_matches('/');
    let Some((_, segment)) = url.rsplit_once('/') else {
        return false;
    };
    let mut chars = segment.chars();
    chars.next() == Some('v')
        && chars.next().is_some_and(|c| c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '.')
}

/// The base URL of an OpenAI Chat Completions endpoint: what is left of
/// `url` without spaces, trailing slashes, or an endpoint's own path
/// (`…/v1/chat/completions` is the base `…/v1`), which would otherwise be
/// asked for `…/chat/completions/chat/completions`.
#[must_use]
pub fn chat_completions_base(url: &str) -> String {
    let mut base = url.trim().trim_end_matches('/');
    for suffix in OPENAI_ENDPOINTS {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped.trim_end_matches('/');
            break;
        }
    }
    base.to_owned()
}

/// `url` as it is saved for a provider speaking `protocol`: without spaces
/// or trailing slashes, and for Chat Completions and Messages without the
/// endpoint's own path.
#[must_use]
pub fn normalized_base_url(protocol: CompatibleProtocol, url: &str) -> String {
    match protocol {
        CompatibleProtocol::OpenAiChatCompletions => chat_completions_base(url),
        CompatibleProtocol::AnthropicMessages => {
            let base = url.trim().trim_end_matches('/');
            base.strip_suffix("/messages")
                .unwrap_or(base)
                .trim_end_matches('/')
                .to_owned()
        }
        _ => url.trim().trim_end_matches('/').to_owned(),
    }
}

/// What the set-up check found out about a custom endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndpointCheck {
    /// The base URL to save: the one given, normalised, or the one under
    /// it that answered (with `/v1` added).
    pub base_url: String,
    /// The models the endpoint listed, when it listed any.
    pub models: Option<Vec<String>>,
    /// What the person should know, one sentence each: what changed, and
    /// what will not work and how to fix it.
    pub notes: Vec<String>,
}

/// How one address answered the model listing.
enum Listing {
    Models(Vec<String>),
    /// It answered as an API, but not with models: a refused key (401, 403)
    /// says the address is right.
    Refused(Option<String>),
    /// No API here: 404, 405, a web page, or something that is not JSON.
    Missing,
    Unreachable(String),
}

/// Check a custom endpoint before it is saved: ask it for its models with
/// `key` (when there is one), at the address given and, when nothing
/// answers there and the address names no version, at `/v1` under it. The
/// check never refuses to save; it says what it found in
/// [`EndpointCheck::notes`].
pub async fn check(
    name: &str,
    protocol: CompatibleProtocol,
    base_url: &str,
    key: Option<&str>,
    headers: &crate::ProviderHeaders,
    models: &[String],
) -> EndpointCheck {
    let given = base_url.trim().trim_end_matches('/');
    let base = normalized_base_url(protocol, base_url);
    let mut result = EndpointCheck {
        base_url: base.clone(),
        ..EndpointCheck::default()
    };
    let client = reqwest::Client::new();
    let listing = match protocol {
        CompatibleProtocol::OpenAiChatCompletions => {
            let first = list(&client, &format!("{base}/models"), key, headers, false).await;
            match first {
                Listing::Missing if !ends_with_version(&base) => {
                    let versioned = format!("{base}/v1");
                    match list(&client, &format!("{versioned}/models"), key, headers, false).await {
                        found @ (Listing::Models(_) | Listing::Refused(..)) => {
                            result.base_url = versioned;
                            found
                        }
                        _ => first,
                    }
                }
                other => other,
            }
        }
        CompatibleProtocol::AnthropicMessages => {
            let endpoint = crate::anthropic::messages_endpoint(&base);
            let root = endpoint.strip_suffix("/messages").unwrap_or(&endpoint);
            list(&client, &format!("{root}/models"), key, headers, true).await
        }
        // The cloud transports and Gemini keep their own checks.
        _ => return result,
    };
    let saved = result.base_url.clone();
    // Where the address is saved, when it is not where it was given.
    if saved != base {
        result.notes.push(format!(
            "Saved the address as {saved}: {name} answers there, not at {base}."
        ));
    } else if base != given {
        result.notes.push(format!(
            "Saved the address as {base}: Haarness adds {} itself.",
            match protocol {
                CompatibleProtocol::AnthropicMessages => "/messages",
                _ => "/chat/completions",
            }
        ));
    }
    match listing {
        Listing::Models(listed) => {
            let missing: Vec<&String> = models
                .iter()
                .filter(|model| !listed.iter().any(|served| served == *model))
                .collect();
            if !missing.is_empty() && !listed.is_empty() {
                result.notes.push(format!(
                    "{name} does not list {}. It lists {}. Chats with a model it does not serve fail; edit the provider's models to match.",
                    join(missing.iter().map(|model| model.as_str())),
                    sample(&listed)
                ));
            } else if listed.is_empty() && models.is_empty() {
                result.notes.push(format!(
                    "{name} lists no models yet. Add the model IDs to use by editing the provider."
                ));
            }
            result.models = Some(listed);
        }
        Listing::Refused(reason) if key.is_some() || !headers.is_empty() => {
            result.notes.push(format!(
                "{name} refused the API key{}. Chats will fail until the key is replaced.",
                reason
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            ));
        }
        // A server may serve chats without listing models, but most that
        // list nothing at this address serve nothing at it either.
        Listing::Missing if protocol == CompatibleProtocol::OpenAiChatCompletions => {
            result.notes.push(format!(
                "Nothing at {saved} answers like an OpenAI-compatible API: it has no model list at {saved}/models. Check the address in the provider's API documentation; it usually ends in /v1."
            ));
        }
        // No key yet: an endpoint that asks for one is where it should be.
        // An Anthropic-compatible gateway may list no models at all.
        Listing::Refused(_) | Listing::Missing => {}
        Listing::Unreachable(cause) => {
            result.notes.push(format!(
                "Haarness could not reach {saved} ({cause}). Check the address, and that the server is running."
            ));
        }
    }
    result
}

async fn list(
    client: &reqwest::Client,
    url: &str,
    key: Option<&str>,
    headers: &crate::ProviderHeaders,
    anthropic: bool,
) -> Listing {
    let mut map: HeaderMap = headers.map();
    if let Some(key) = key.map(str::trim).filter(|key| !key.is_empty()) {
        if anthropic {
            if let Ok(value) = HeaderValue::from_str(key) {
                map.insert("x-api-key", value);
            }
            map.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {key}")) {
            map.insert(AUTHORIZATION, value);
        }
    }
    let response = match client.get(url).headers(map).timeout(TIMEOUT).send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return Listing::Unreachable("no answer in time".into());
        }
        Err(error) => {
            let cause =
                std::iter::successors(std::error::Error::source(&error), |cause| cause.source())
                    .last()
                    .map_or_else(|| "no connection".to_owned(), ToString::to_string);
            return Listing::Unreachable(cause);
        }
    };
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let json = serde_json::from_str::<Value>(&body).ok();
    match (status, json) {
        (200..=299, Some(value)) => match value["data"].as_array() {
            Some(entries) => Listing::Models(
                entries
                    .iter()
                    .filter_map(|entry| entry["id"].as_str().map(str::to_owned))
                    .collect(),
            ),
            None => Listing::Missing,
        },
        (401 | 403, json) => Listing::Refused(
            json.as_ref()
                .and_then(|value| {
                    ["/error/message", "/message", "/error", "/detail", "/msg"]
                        .iter()
                        .find_map(|pointer| value.pointer(pointer)?.as_str())
                })
                .map(|reason| {
                    let reason: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
                    reason.chars().take(160).collect()
                }),
        ),
        _ => Listing::Missing,
    }
}

fn join<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let items: Vec<_> = items.collect();
    match items.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// A few of `listed`, enough to see what the endpoint's model IDs look like.
fn sample(listed: &[String]) -> String {
    const SHOWN: usize = 6;
    let shown = listed
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if listed.len() > SHOWN {
        format!("{shown} and {} more", listed.len() - SHOWN)
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::get};

    #[test]
    fn a_pasted_endpoint_becomes_its_base() {
        for (url, base) in [
            ("https://api.deepseek.com/v1", "https://api.deepseek.com/v1"),
            (
                "https://api.deepseek.com/v1/",
                "https://api.deepseek.com/v1",
            ),
            (
                " https://api.deepseek.com/v1/chat/completions\n",
                "https://api.deepseek.com/v1",
            ),
            (
                "https://openrouter.ai/api/v1/chat/completions/",
                "https://openrouter.ai/api/v1",
            ),
            (
                "http://127.0.0.1:1234/v1/models",
                "http://127.0.0.1:1234/v1",
            ),
            (
                "https://api.openai.com/v1/responses",
                "https://api.openai.com/v1",
            ),
            (
                "https://api.z.ai/api/paas/v4",
                "https://api.z.ai/api/paas/v4",
            ),
        ] {
            assert_eq!(chat_completions_base(url), base, "{url}");
        }
        assert_eq!(
            normalized_base_url(
                CompatibleProtocol::AnthropicMessages,
                "https://api.z.ai/api/anthropic/v1/messages/"
            ),
            "https://api.z.ai/api/anthropic/v1"
        );
        assert_eq!(
            normalized_base_url(
                CompatibleProtocol::AnthropicMessages,
                "https://api.z.ai/api/anthropic"
            ),
            "https://api.z.ai/api/anthropic"
        );
    }

    #[test]
    fn versions_are_the_last_segment() {
        assert!(ends_with_version("https://api.groq.com/openai/v1"));
        assert!(ends_with_version("https://api.z.ai/api/paas/v4/"));
        assert!(ends_with_version(
            "https://generativelanguage.googleapis.com/v1beta"
        ));
        assert!(!ends_with_version("https://api.groq.com/openai"));
        assert!(!ends_with_version("https://api.deepseek.com"));
        assert!(!ends_with_version("https://example.com/vendor"));
    }

    /// An API served under `/openai/v1`, as Groq's is, that wants `secret`.
    async fn groq_like() -> String {
        async fn models(headers: axum::http::HeaderMap) -> (StatusCode, Json<Value>) {
            if headers
                .get(AUTHORIZATION)
                .is_some_and(|value| value == "Bearer secret")
            {
                (
                    StatusCode::OK,
                    Json(serde_json::json!({"data":[{"id":"fast-model"},{"id":"big-model"}]})),
                )
            } else {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error":{"message":"Invalid API Key"}})),
                )
            }
        }
        let router = Router::new()
            .route("/openai/v1/models", get(models))
            .fallback(|| async {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error":{"message":"Unknown request URL"}})),
                )
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    fn headers() -> crate::ProviderHeaders {
        crate::ProviderHeaders::default()
    }

    #[tokio::test]
    async fn an_address_missing_its_version_is_saved_where_the_api_answers() {
        let root = groq_like().await;
        let checked = check(
            "Groq",
            CompatibleProtocol::OpenAiChatCompletions,
            &format!("{root}/openai"),
            Some("secret"),
            &headers(),
            &["fast-model".into()],
        )
        .await;
        assert_eq!(checked.base_url, format!("{root}/openai/v1"));
        assert_eq!(
            checked.models,
            Some(vec!["fast-model".into(), "big-model".into()])
        );
        assert_eq!(
            checked.notes,
            [format!(
                "Saved the address as {root}/openai/v1: Groq answers there, not at {root}/openai."
            )]
        );
        // The endpoint pasted whole, with a slash.
        let checked = check(
            "Groq",
            CompatibleProtocol::OpenAiChatCompletions,
            &format!("{root}/openai/v1/chat/completions/"),
            None,
            &headers(),
            &[],
        )
        .await;
        assert_eq!(checked.base_url, format!("{root}/openai/v1"));
        assert_eq!(
            checked.notes,
            [format!(
                "Saved the address as {root}/openai/v1: Haarness adds /chat/completions itself."
            )]
        );
    }

    #[tokio::test]
    async fn the_check_says_what_will_not_work() {
        let root = groq_like().await;
        let refused = check(
            "Groq",
            CompatibleProtocol::OpenAiChatCompletions,
            &format!("{root}/openai/v1"),
            Some("wrong"),
            &headers(),
            &[],
        )
        .await;
        assert_eq!(
            refused.notes,
            [
                "Groq refused the API key (Invalid API Key). Chats will fail until the key is replaced."
            ]
        );
        let unlisted = check(
            "Groq",
            CompatibleProtocol::OpenAiChatCompletions,
            &format!("{root}/openai/v1"),
            Some("secret"),
            &headers(),
            &["fast-model".into(), "deepseek-chat".into()],
        )
        .await;
        assert_eq!(
            unlisted.notes,
            [
                "Groq does not list deepseek-chat. It lists fast-model, big-model. Chats with a model it does not serve fail; edit the provider's models to match."
            ]
        );
        let nowhere = check(
            "Groq",
            CompatibleProtocol::OpenAiChatCompletions,
            &format!("{root}/elsewhere/v2"),
            Some("secret"),
            &headers(),
            &[],
        )
        .await;
        assert_eq!(nowhere.base_url, format!("{root}/elsewhere/v2"));
        assert_eq!(
            nowhere.notes,
            [format!(
                "Nothing at {root}/elsewhere/v2 answers like an OpenAI-compatible API: it has no model list at {root}/elsewhere/v2/models. Check the address in the provider's API documentation; it usually ends in /v1."
            )]
        );
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let offline = check(
            "Local",
            CompatibleProtocol::OpenAiChatCompletions,
            &format!("http://127.0.0.1:{port}/v1"),
            None,
            &headers(),
            &[],
        )
        .await;
        assert_eq!(offline.notes.len(), 1, "{:?}", offline.notes);
        assert!(
            offline.notes[0].starts_with(&format!(
                "Haarness could not reach http://127.0.0.1:{port}/v1 ("
            )),
            "{:?}",
            offline.notes
        );
    }
}
