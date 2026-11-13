//! Why a turn failed, read back from the error it stored, as one of a closed
//! list of kinds. Usage statistics report the kind with the scrubbed words, so
//! failures can be counted by kind across wordings and versions.
//!
//! The stored error is the transcript's words for the failure
//! ([`turn_failure_words`](super::turn_failure_words)), so the kind is read
//! from Haarness's own wording first and from the provider's words it keeps
//! after that. The tests below fail the turns these words come from, so a
//! change of wording that loses its kind shows.

use serde::{Deserialize, Serialize};

use crate::CoreError;

/// Why a turn failed. The wire names ([`FailureKind::as_str`]) are the list
/// Haarness Cloud's usage report takes; a kind added here is one Cloud should
/// learn first (until then it counts there as `other`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// A limit on requests or tokens a minute; waiting clears it.
    RateLimited,
    /// A day's allowance used up, or an account out of credit or over its
    /// budget.
    Quota,
    /// A provider not connected, or a key or sign-in it refused or that
    /// expired.
    Auth,
    /// A model retired, outside the plan or region, or not downloaded.
    ModelUnavailable,
    /// An attachment the model can't take.
    UnsupportedInput,
    /// The chat or the message is too long for the model.
    ContextOverflow,
    /// The provider was overloaded or unavailable, even after retries.
    Overloaded,
    /// The provider answered with a server error.
    ProviderError,
    /// The provider refused the request as invalid.
    InvalidRequest,
    /// The configured address doesn't speak the provider's API.
    Endpoint,
    /// The provider couldn't be reached, or the connection broke.
    Network,
    /// The provider answered nothing in time.
    Timeout,
    /// Answers kept ending before they were done.
    Incomplete,
    /// The model sent back an empty reply.
    EmptyReply,
    /// The provider's safety check stopped the response.
    Blocked,
    /// The model's tool calls couldn't be read or didn't match their tools.
    ToolCallUnreadable,
    /// Compacting the chat failed.
    Compaction,
    /// A hook refused the prompt.
    Hook,
    /// Haarness's own fault.
    Internal,
    /// Anything else.
    Other,
}

impl FailureKind {
    pub const ALL: [Self; 20] = [
        Self::RateLimited,
        Self::Quota,
        Self::Auth,
        Self::ModelUnavailable,
        Self::UnsupportedInput,
        Self::ContextOverflow,
        Self::Overloaded,
        Self::ProviderError,
        Self::InvalidRequest,
        Self::Endpoint,
        Self::Network,
        Self::Timeout,
        Self::Incomplete,
        Self::EmptyReply,
        Self::Blocked,
        Self::ToolCallUnreadable,
        Self::Compaction,
        Self::Hook,
        Self::Internal,
        Self::Other,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Quota => "quota",
            Self::Auth => "auth",
            Self::ModelUnavailable => "model_unavailable",
            Self::UnsupportedInput => "unsupported_input",
            Self::ContextOverflow => "context_overflow",
            Self::Overloaded => "overloaded",
            Self::ProviderError => "provider_error",
            Self::InvalidRequest => "invalid_request",
            Self::Endpoint => "endpoint",
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::Incomplete => "incomplete",
            Self::EmptyReply => "empty_reply",
            Self::Blocked => "blocked",
            Self::ToolCallUnreadable => "tool_call_unreadable",
            Self::Compaction => "compaction",
            Self::Hook => "hook",
            Self::Internal => "internal",
            Self::Other => "other",
        }
    }

    /// The kind of failure a failed turn's stored error says happened.
    #[must_use]
    pub fn of(turn_error: &str) -> Self {
        let text = turn_error.trim();
        // A turn that couldn't start stores the error after a prefix.
        if let Some(error) = text.strip_prefix(super::LAUNCH_FAILED) {
            return Self::of(error);
        }
        if text.is_empty() {
            return Self::Other;
        }
        let lower = text.to_ascii_lowercase();
        let says = |phrases: &[&str]| phrases.iter().any(|phrase| lower.contains(phrase));
        let begins = |phrases: &[&str]| phrases.iter().any(|phrase| lower.starts_with(phrase));

        // Haarness's own failures, which aren't a provider's answer: what
        // they quote (a hook's reason, a panic's message) says nothing more.
        if super::prompt_blocked(text).is_some() || lower.starts_with(super::PROMPT_BLOCKED) {
            return Self::Hook;
        }
        if text.starts_with(super::retained::MESSAGE_TOO_LONG) {
            return Self::ContextOverflow;
        }
        if says(&["haarness stopped this turn after an internal error"])
            || begins(&[
                "invalid request: ",
                "not found: ",
                "conflict: ",
                "permission denied: ",
                "storage error: ",
                "tool error: ",
            ])
        {
            return Self::Internal;
        }
        if lower.starts_with("the provider's safety check stopped this response") {
            return Self::Blocked;
        }

        // The runtime's words for what went wrong, most specific first.
        if lower.starts_with("provider response ended before completion")
            || says(&["kept ending its answer before it was done"])
        {
            return Self::Incomplete;
        }
        if says(&["didn't write a summary of this chat"]) {
            return Self::Compaction;
        }
        if says(&[
            "couldn't read the model's tool calls",
            "tool calls kept breaking off",
            "refused the model's tool calls",
        ]) {
            return Self::ToolCallUnreadable;
        }
        if says(&["sent back an empty reply"]) {
            return Self::EmptyReply;
        }
        if says(&[". send the message without the ", " doesn't read images"]) {
            return Self::UnsupportedInput;
        }
        if says(&[
            "has no chat api at ",
            "nothing at this address speaks its api",
        ]) {
            return Self::Endpoint;
        }
        if says(&[
            "is not connected. connect it",
            "isn't connected. connect it",
            "rejected the api key or sign-in",
            "did not accept this api key",
            "sign-in has expired or was ended elsewhere",
            "couldn't renew your sign-in",
            "isn't signed in to ollama.com",
            "has no copilot plan",
        ]) {
            return Self::Auth;
        }
        if says(&[
            "no longer offers ",
            "isn't included with a chatgpt sign-in",
            "isn't included with this chatgpt sign-in",
            "plan doesn't include",
            "free tier doesn't include",
            "isn't downloaded in ollama",
            "can't use tools in ollama",
        ]) || super::is_region_unavailable(text)
        {
            return Self::ModelUnavailable;
        }
        if says(&[
            "this chat is too large for ",
            "refused this request as too large",
            "tokens at once here",
        ]) {
            return Self::ContextOverflow;
        }
        if says(&[
            "used up today's",
            "has used today's",
            "has used up ",
            "out of credit",
            "has no credit left",
        ]) || super::cloud_billing_advice(text)
        {
            return Self::Quota;
        }
        if says(&[
            "is at the limit of ",
            "tokens a minute",
            "pending-request allowance is full",
        ]) {
            return Self::RateLimited;
        }
        if let Some(at) = lower.find("rate limit or quota reached") {
            // The provider's own words, when the runtime kept them, say which.
            return match lower[at..].split_once(": ") {
                Some((_, reason)) => quota_or_rate_limit(reason),
                None => Self::RateLimited,
            };
        }
        if says(&[
            "is overloaded or unavailable right now",
            "can't reach this model's provider right now",
            "holding requests to it",
        ]) {
            return Self::Overloaded;
        }
        if lower.starts_with("could not reach ")
            || says(&[
                "kept breaking off mid-answer",
                "broke off before it answered",
                "isn't running. start it",
            ])
        {
            return Self::Network;
        }
        if says(&[" sent nothing for ", "did not respond in time"]) {
            return Self::Timeout;
        }

        // The rest keep the provider's words ("Provider error: HTTP 500 …",
        // "Anthropic rejected the request: prompt is too long …"), which the
        // runtime's own tests of a provider's error read.
        let provider = CoreError::Provider(text.to_owned());
        if super::is_provider_context_overflow(&provider)
            || super::is_provider_oversized_request(&provider)
        {
            return Self::ContextOverflow;
        }
        if super::is_provider_usage_limit(&provider) {
            return quota_or_rate_limit(&lower);
        }
        if super::is_provider_image_rejection(&provider, true) {
            return Self::UnsupportedInput;
        }
        if super::refused_tool_call(&provider).is_some() {
            return Self::ToolCallUnreadable;
        }
        if says(&[
            "model_not_found",
            "model not found",
            "model_not_supported",
            "unknown model",
            "no such model",
            "model does not exist",
            "model is not supported",
            "is not a valid model",
        ]) {
            return Self::ModelUnavailable;
        }
        if super::is_transport_cut(text)
            || says(&[
                "error sending request",
                "connection refused",
                "connection reset",
                "dns error",
            ])
        {
            return Self::Network;
        }
        if crate::stream_stall_silence(&provider).is_some()
            || says(&["timed out", "timeout", "no complete response within"])
        {
            return Self::Timeout;
        }
        // A refused key that the provider answers as a bad request (Google
        // sends 400 INVALID_ARGUMENT with API_KEY_INVALID).
        if says(&[
            "api key not valid",
            "api_key_invalid",
            "invalid api key",
            "incorrect api key",
            "invalid x-api-key",
            "invalid_api_key",
        ]) {
            return Self::Auth;
        }
        match super::stop_failure_categories(&provider).1 {
            "authentication_failed" => return Self::Auth,
            "rate_limit" => return quota_or_rate_limit(&lower),
            "overloaded" => return Self::Overloaded,
            "server_error" => return Self::ProviderError,
            "invalid_request" => return Self::InvalidRequest,
            _ => {}
        }
        if lower.starts_with("provider rejected the request") || says(&[" rejected the request: "])
        {
            return Self::InvalidRequest;
        }
        if lower.starts_with("provider error") {
            return Self::ProviderError;
        }
        Self::Other
    }
}

/// A usage limit in a provider's own words: a day's allowance, a quota or an
/// account's money, or else a limit a minute's wait clears.
fn quota_or_rate_limit(lower: &str) -> FailureKind {
    let minute = [
        "per minute",
        "perminute",
        "per_minute",
        "tokens a minute",
        "(tpm)",
        "(rpm)",
    ];
    if minute.iter().any(|words| lower.contains(words)) {
        return FailureKind::RateLimited;
    }
    let account = [
        "quota",
        "billing",
        "credit",
        "balance",
        "per day",
        "perday",
        "daily",
        "spending limit",
        "budget",
    ];
    if account.iter().any(|words| lower.contains(words)) {
        FailureKind::Quota
    } else {
        FailureKind::RateLimited
    }
}

#[cfg(test)]
mod tests {
    use haarness_protocol::{ModelInfo, ProviderInfo};

    use super::FailureKind::{self, *};
    use crate::CoreError;

    fn model(id: &str, tools: bool) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: false,
            reasoning_efforts: Vec::new(),
            images: false,
            video: false,
            tools,
            context_window: Some(32_768),
            max_output_tokens: None,
            pricing: None,
        }
    }

    fn provider(id: &str, name: &str, models: &[&str]) -> ProviderInfo {
        ProviderInfo {
            id: id.into(),
            name: name.into(),
            authenticated: true,
            models: models.iter().map(|id| model(id, true)).collect(),
        }
    }

    /// The kind of a turn on `model` of `info` that failed with `error`, read
    /// from the words it stores, as a person's turn (with advice) and as a
    /// subagent's (without).
    fn kinds(error: &CoreError, info: Option<&ProviderInfo>, model: &str) -> [FailureKind; 2] {
        [true, false].map(|advice| {
            FailureKind::of(&super::super::turn_failure_words(
                error, info, model, advice,
            ))
        })
    }

    #[test]
    fn every_kind_has_one_wire_name() {
        let names = FailureKind::ALL.map(FailureKind::as_str);
        assert_eq!(
            names,
            [
                "rate_limited",
                "quota",
                "auth",
                "model_unavailable",
                "unsupported_input",
                "context_overflow",
                "overloaded",
                "provider_error",
                "invalid_request",
                "endpoint",
                "network",
                "timeout",
                "incomplete",
                "empty_reply",
                "blocked",
                "tool_call_unreadable",
                "compaction",
                "hook",
                "internal",
                "other",
            ],
            "Haarness Cloud's usage report lists the same kinds"
        );
        for kind in FailureKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.as_str())
            );
        }
    }

    #[test]
    fn provider_failures_are_read_back_from_the_words_turns_store() {
        let anthropic = provider("anthropic", "Anthropic", &["claude-sonnet-5-5"]);
        let openai = provider("openai", "OpenAI", &["gpt-6.1-sol", "gpt-6-luna"]);
        let gemini = provider(
            "gemini",
            "Gemini",
            &["gemini-3-flash-preview", "gemini-4-argon"],
        );
        let groq = provider("groq", "Groq", &["openai/gpt-oss-120b"]);
        let custom = provider("acme", "Acme", &[]);
        let cases: Vec<(&str, &ProviderInfo, &str, FailureKind)> = vec![
            // A key the provider refuses as a bad request.
            (
                r#"HTTP 400 Bad Request: {"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID","domain":"googleapis.com"}]}}"#,
                &gemini,
                "gemini-3-flash-preview",
                Auth,
            ),
            // A minute's limits.
            (
                "HTTP 429 Too Many Requests: rate limit exceeded, retry later",
                &anthropic,
                "claude-sonnet-5-5",
                RateLimited,
            ),
            (
                "HTTP 429 Too Many Requests",
                &anthropic,
                "claude-sonnet-5-5",
                RateLimited,
            ),
            (
                "HTTP 429 Too Many Requests: Request too large for model `openai/gpt-oss-120b` in organization `org_1` service tier `on_demand` on tokens per minute (TPM): Limit 8000, Requested 11419, please reduce your message size and try again. Need more tokens? Upgrade to Dev Tier today at https://console.groq.com/settings/billing",
                &groq,
                "openai/gpt-oss-120b",
                RateLimited,
            ),
            (
                "HTTP 429 Too Many Requests: You exceeded your current quota, please check your plan and billing details.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 5, model: gemini-3-flash\nPlease retry in 21.2s.",
                &gemini,
                "gemini-3-flash-preview",
                RateLimited,
            ),
            // A day's allowance, an account's money.
            (
                "HTTP 429 Too Many Requests: You exceeded your current quota, please check your plan and billing details.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 20, model: gemini-3-flash\nPlease retry in 11.6s. Quota id: GenerateRequestsPerDayPerProjectPerModel-FreeTier",
                &gemini,
                "gemini-3-flash-preview",
                Quota,
            ),
            (
                "HTTP 429 Too Many Requests: [insufficient_quota] You exceeded your current quota, please check your plan and billing details.",
                &openai,
                "gpt-6.1-sol",
                Quota,
            ),
            (
                "HTTP 429 Too Many Requests: [RESOURCE_EXHAUSTED] You exceeded your current quota, please check your plan and billing details.",
                &custom,
                "acme-1",
                Quota,
            ),
            (
                "HTTP 402 Payment Required: Insufficient Balance",
                &custom,
                "acme-1",
                Quota,
            ),
            (
                "Your included Haarness Cloud credits can't cover this request. Add credits or turn on overage.",
                &anthropic,
                "claude-sonnet-5-5",
                Quota,
            ),
            // Keys and sign-ins.
            (
                "HTTP 401 Unauthorized: invalid x-api-key",
                &anthropic,
                "claude-sonnet-5-5",
                Auth,
            ),
            (
                "Anthropic API key is not configured; store a credential for anthropic or set its api_key_env",
                &anthropic,
                "claude-sonnet-5-5",
                Auth,
            ),
            (
                "OpenAI sign-in failed: refresh token returned 401 Unauthorized",
                &openai,
                "gpt-6.1-sol",
                Auth,
            ),
            (
                "OpenAI sign-in failed: refresh token returned 503 Service Unavailable",
                &openai,
                "gpt-6.1-sol",
                Auth,
            ),
            // Models.
            (
                "HTTP 400 Bad Request: [model_not_found] The 'gpt-5.4-mini' model is not supported when using Codex with a ChatGPT account.",
                &openai,
                "gpt-5.4-mini",
                ModelUnavailable,
            ),
            (
                "HTTP 429 Too Many Requests: You exceeded your current quota, please check your plan and billing details.\n* Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_input_token_count, limit: 0, model: gemini-4-argon\nPlease retry in 41.236318127s.",
                &gemini,
                "gemini-4-argon",
                ModelUnavailable,
            ),
            (
                "HTTP 404 Not Found: This model models/gemini-2.5-pro is no longer available to new users. Please update your code to use models/gemini-4-argon.",
                &gemini,
                "gemini-2.5-pro",
                ModelUnavailable,
            ),
            (
                "HTTP 403 Forbidden: [unsupported_country_region_territory] Country, region, or territory not supported",
                &openai,
                "gpt-6.1-sol",
                ModelUnavailable,
            ),
            (
                "HTTP 404 Not Found: {\"error\":{\"message\":\"The model `acme-9` does not exist\",\"type\":\"invalid_request_error\",\"code\":\"model_not_found\"}}",
                &custom,
                "acme-9",
                ModelUnavailable,
            ),
            // Too long.
            (
                "HTTP 400 Bad Request: prompt is too long: 250000 tokens > 200000 maximum",
                &anthropic,
                "claude-sonnet-5-5",
                ContextOverflow,
            ),
            (
                "request still too large after compacting: HTTP 413 Payload Too Large: request entity too large",
                &anthropic,
                "claude-sonnet-5-5",
                ContextOverflow,
            ),
            (
                "{\"error\":{\"message\":\"This model's maximum context length is 128000 tokens.\",\"type\":\"invalid_request_error\",\"code\":\"context_length_exceeded\"}}",
                &custom,
                "acme-1",
                ContextOverflow,
            ),
            // Busy, broken, slow.
            (
                "HTTP 529: Overloaded",
                &anthropic,
                "claude-sonnet-5-5",
                Overloaded,
            ),
            (
                "HTTP 503 Service Unavailable: The model is overloaded. Please try again later.",
                &gemini,
                "gemini-3-flash-preview",
                Overloaded,
            ),
            (
                "[no_route_available] The router is holding requests to it after several failures.",
                &anthropic,
                "claude-sonnet-5-5",
                Overloaded,
            ),
            (
                "HTTP 500 Internal Server Error: something broke",
                &custom,
                "acme-1",
                ProviderError,
            ),
            (
                "provider ended the response with an error",
                &custom,
                "acme-1",
                ProviderError,
            ),
            (
                "HTTP 400 Bad Request: [invalid_request_error] messages.3: tool_use ids must be unique",
                &anthropic,
                "claude-sonnet-5-5",
                InvalidRequest,
            ),
            (
                "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"temperature is not supported\"}}",
                &custom,
                "acme-1",
                InvalidRequest,
            ),
            (
                "error sending request for url (http://localhost:8080/v1/chat/completions): connection refused",
                &custom,
                "acme-1",
                Network,
            ),
            (
                "error decoding response body: connection reset by peer",
                &custom,
                "acme-1",
                Network,
            ),
            ("operation timed out", &custom, "acme-1", Timeout),
            // The model's own output.
            (
                "HTTP 400 Bad Request: [tool_use_failed] Tool call validation failed: parameters for tool apply_patch did not match schema",
                &groq,
                "openai/gpt-oss-120b",
                ToolCallUnreadable,
            ),
            (
                "Gemma doesn't read images. Send the message without the picture, or switch to a model that reads them.",
                &custom,
                "gemma",
                UnsupportedInput,
            ),
        ];
        for (raw, info, model, kind) in cases {
            let error = CoreError::Provider(raw.into());
            assert_eq!(
                kinds(&error, Some(info), model),
                [kind; 2],
                "{raw}\n{}",
                super::super::turn_failure_words(&error, Some(info), model, true)
            );
        }
        // Words that name no provider read the same.
        assert_eq!(
            kinds(
                &CoreError::Provider("HTTP 401 Unauthorized".into()),
                None,
                "x"
            ),
            [Auth; 2]
        );
        assert_eq!(
            kinds(
                &crate::stream_stall(std::time::Duration::from_secs(300)),
                None,
                "x"
            ),
            [Timeout; 2]
        );
        assert_eq!(
            kinds(
                &crate::response_timeout(std::time::Duration::from_secs(600)),
                None,
                "x"
            ),
            [Timeout; 2]
        );
    }

    #[test]
    fn haarness_failures_are_read_back_from_the_words_turns_store() {
        let custom = provider("acme", "Acme", &[]);
        let unreadable = format!("{}invalid character 'x'", crate::UNREADABLE_TOOL_CALL);
        let cut = format!(
            "{}unexpected end of JSON input",
            crate::UNREADABLE_TOOL_CALL
        );
        let blocked = format!("{}secrets in the prompt", super::super::PROMPT_BLOCKED);
        let too_long = format!(
            "{} (it needs 300,000 tokens).",
            super::super::retained::MESSAGE_TOO_LONG
        );
        let cases: Vec<(CoreError, FailureKind)> = vec![
            (CoreError::Provider(unreadable), ToolCallUnreadable),
            (CoreError::Provider(cut), ToolCallUnreadable),
            (
                CoreError::Provider(super::super::EMPTY_SUMMARY.into()),
                Compaction,
            ),
            (CoreError::ProviderBlocked("content_filter".into()), Blocked),
            (
                CoreError::ProviderIncomplete("missing completion event".into()),
                Incomplete,
            ),
            (CoreError::ProviderIncomplete("length".into()), Incomplete),
            (
                CoreError::ProviderIncomplete(
                    "error decoding response body: connection reset by peer".into(),
                ),
                Network,
            ),
            (CoreError::Invalid(blocked), Hook),
            (CoreError::Invalid(too_long), ContextOverflow),
            (CoreError::Invalid("Unknown agent: nope".into()), Internal),
            (CoreError::Storage("database is locked".into()), Internal),
            (
                super::super::turn_panicked(&"index out of bounds: the len is 0"),
                Internal,
            ),
            (
                CoreError::ProviderUnavailable(
                    "Ollama isn't running. Start it on the computer this chat runs on, then send your message again.".into(),
                ),
                Network,
            ),
            (
                CoreError::ProviderUnavailable(
                    "Provider acme isn't connected. Connect it, or pick another model.".into(),
                ),
                Auth,
            ),
            (
                CoreError::ProviderUnavailable(
                    "Acme has no chat API at http://localhost:8080. Change the provider's base URL to the one in its API documentation: it usually ends in /v1.".into(),
                ),
                Endpoint,
            ),
            (
                CoreError::ProviderUnavailable(
                    "The provider answered without a reply, so nothing at this address speaks its API. Check the provider's base URL against its API documentation.".into(),
                ),
                Endpoint,
            ),
            (
                CoreError::ProviderUnavailable(
                    "Your ChatGPT Free plan doesn't include Codex, which Haarness uses when you sign in with ChatGPT. Upgrade the plan at chatgpt.com/pricing, or connect OpenAI with an API key instead.".into(),
                ),
                ModelUnavailable,
            ),
        ];
        for (error, kind) in cases {
            assert_eq!(
                kinds(&error, Some(&custom), "acme-1"),
                [kind; 2],
                "{error:?}\n{}",
                super::super::turn_failure_words(&error, Some(&custom), "acme-1", true)
            );
        }
        // A turn that couldn't start stores its error after a prefix.
        let launch = format!(
            "{}{}",
            super::super::LAUNCH_FAILED,
            CoreError::Storage("disk full".into())
        );
        assert_eq!(FailureKind::of(&launch), Internal);
        for nothing in ["", "  ", "Something nobody has seen before."] {
            assert_eq!(FailureKind::of(nothing), Other, "{nothing:?}");
        }
    }

    #[test]
    fn local_model_failures_are_read_back_from_the_words_turns_store() {
        let mut ollama = provider("ollama", "Ollama", &["qwen3:8b", "llama3:8b"]);
        ollama.models.push(model("gemma3:latest", false));
        let lmstudio = provider("lmstudio", "LM Studio", &["qwen3-0.6b"]);
        let empty = format!("{}qwen3:0.6b", super::super::EMPTY_LOCAL_REPLY);
        for (raw, info, model, kind) in [
            (
                "HTTP 404 Not Found: model 'qwen2.5-coder:14b' not found. Download it with `ollama pull qwen2.5-coder:14b`, or pick a model you have.",
                &ollama,
                "qwen2.5-coder:14b",
                ModelUnavailable,
            ),
            (
                "HTTP 400 Bad Request: registry.ollama.ai/library/gemma3:latest does not support tools",
                &ollama,
                "gemma3:latest",
                ModelUnavailable,
            ),
            (
                "HTTP 401 Unauthorized: unauthorized",
                &ollama,
                "gpt-oss:120b-cloud",
                Auth,
            ),
            (empty.as_str(), &ollama, "qwen3:0.6b", EmptyReply),
            (empty.as_str(), &lmstudio, "qwen3-0.6b", EmptyReply),
            (
                "HTTP 500 Internal Server Error: prompt too long; exceeded max context length by 1520 tokens",
                &ollama,
                "llama3:8b",
                ContextOverflow,
            ),
        ] {
            let error = CoreError::Provider(raw.into());
            assert_eq!(
                kinds(&error, Some(info), model),
                [kind; 2],
                "{raw}\n{}",
                super::super::turn_failure_words(&error, Some(info), model, true)
            );
        }
    }
}
