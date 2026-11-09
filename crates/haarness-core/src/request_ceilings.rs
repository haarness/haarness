//! What one request may hold on a provider account whose plan allows only a
//! few thousand tokens a minute (Groq's free tier: 8,000 for most models,
//! 7,000 input and 1,000 output for some), learned from what the provider
//! says of it: a refusal of a request too large for the minute, or a rate
//! limit that names it while asking for a wait.
//!
//! Such a ceiling is far below the model's context window, and no wait lets
//! a larger request through. Haarness's optional tools then wait behind
//! `load_tool`, as they do in a small context window (about half of every
//! request), and a reply is asked for no more output than the minute allows,
//! so the turn goes on instead of failing, and more requests fit a minute.

use dashmap::DashMap;

/// A refusal of one request as larger than the account's tokens a minute,
/// as the provider counted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PerMinuteRefusal {
    /// The prompt (Groq's TPM or ITPM): `requested` input tokens against
    /// `limit`.
    Input { limit: u64, requested: u64 },
    /// The reply the provider expects (Groq's OTPM): `requested` output
    /// tokens against `limit`.
    Output { limit: u64, requested: u64 },
}

/// The fewest tokens a minute treated as a ceiling to fit under: below this
/// no coding request fits whatever is left out, and the refusal stands.
const LEAST_INPUT_CEILING: u64 = 2_048;
/// No reply is asked to be shorter than this.
const LEAST_OUTPUT_CEILING: u64 = 256;

/// A provider's words about the account's tokens a minute for one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PerMinuteLimit {
    pub refusal: PerMinuteRefusal,
    /// The request alone is over the limit ("Request too large"), rather
    /// than over what is left of this minute ("Rate limit reached"), which a
    /// wait clears.
    pub too_large: bool,
}

/// Reads what a provider says of the account's tokens a minute: Groq's
/// "Request too large for model `…` … on tokens per minute (TPM): Limit
/// 8000, Requested 11419", "… on input tokens per minute (ITPM): Limit 7000,
/// Requested 7185" and "… on output tokens per minute (OTPM): Limit 1000,
/// Requested 1404", or a rate limit that asks for a wait ("Rate limit
/// reached … (TPM): Limit 8000, Used 5410, Requested 5380 … try again in
/// 20.9s"), which names the same limit.
pub(crate) fn per_minute_limit(message: &str) -> Option<PerMinuteLimit> {
    let lower = message.to_ascii_lowercase();
    let too_large = lower.contains("request too large");
    // OpenAI writes "tokens per min (TPM)".
    if !(too_large || lower.contains("rate limit reached")) || !lower.contains("tokens per min") {
        return None;
    }
    // The number right after a label ("Rate limit reached" has "limit "
    // with none after it).
    let number = |label: &str| {
        lower.match_indices(label).find_map(|(at, _)| {
            lower[at + label.len()..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|digits| digits.parse::<u64>().ok())
        })
    };
    let limit = number("limit ")?;
    let requested = number("requested ")?;
    if too_large && requested <= limit {
        return None;
    }
    let refusal = if lower.contains("output tokens per min") {
        PerMinuteRefusal::Output { limit, requested }
    } else {
        PerMinuteRefusal::Input { limit, requested }
    };
    Some(PerMinuteLimit { refusal, too_large })
}

/// A provider's refusal of a request too large for the account's tokens a
/// minute (see [`per_minute_limit`]): no wait lets it through.
pub(crate) fn per_minute_refusal(message: &str) -> Option<PerMinuteRefusal> {
    per_minute_limit(message)
        .filter(|limit| limit.too_large)
        .map(|limit| limit.refusal)
}

/// The ceilings learned for one model on one provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Ceiling {
    /// The most prompt tokens one request may carry.
    pub input: Option<u64>,
    /// The most output tokens one reply may be asked for.
    pub output: Option<u64>,
}

impl Ceiling {
    /// The window the tool policy sizes this model's tools for: its context
    /// window, or the account's ceiling where that is smaller.
    pub fn tool_window(self, context_window: Option<u64>) -> Option<u64> {
        match (context_window.filter(|window| *window > 0), self.input) {
            (Some(window), Some(ceiling)) => Some(window.min(ceiling)),
            (None, Some(ceiling)) => Some(ceiling),
            (window, None) => window,
        }
    }
}

/// Ceilings learned on this computer since Haarness started, per provider
/// and model, shared by every chat: a new chat on the same account starts
/// within them. A plan upgraded meanwhile only costs tools loaded on demand
/// until the next start.
#[derive(Debug, Default)]
pub(crate) struct RequestCeilings {
    learned: DashMap<(String, String), Ceiling>,
}

impl RequestCeilings {
    pub fn get(&self, provider: &str, model: &str) -> Ceiling {
        self.learned
            .get(&(provider.to_owned(), model.to_owned()))
            .map(|ceiling| *ceiling)
            .unwrap_or_default()
    }

    /// Learns `refusal` for `model` on `provider`. True when the next
    /// request can be made to fit where this one did not: a ceiling it did
    /// not know, or a lower one than it knew.
    pub fn learn(&self, provider: &str, model: &str, refusal: PerMinuteRefusal) -> bool {
        let mut ceiling = self
            .learned
            .entry((provider.to_owned(), model.to_owned()))
            .or_default();
        let (known, limit, least) = match refusal {
            PerMinuteRefusal::Input { limit, .. } => {
                (&mut ceiling.input, limit, LEAST_INPUT_CEILING)
            }
            PerMinuteRefusal::Output { limit, .. } => {
                (&mut ceiling.output, limit, LEAST_OUTPUT_CEILING)
            }
        };
        if limit < least || known.is_some_and(|known| known <= limit) {
            return false;
        }
        *known = Some(limit);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Ceiling, PerMinuteLimit, PerMinuteRefusal, RequestCeilings, per_minute_limit,
        per_minute_refusal,
    };

    const TPM: &str = "HTTP 413 Payload Too Large: Request too large for model `openai/gpt-oss-120b` in organization `org_1` service tier `on_demand` on tokens per minute (TPM): Limit 8000, Requested 8421, please reduce your message size and try again. Need more tokens? Upgrade to Dev Tier today at https://console.groq.com/settings/billing";
    const ITPM: &str = "HTTP 413 Payload Too Large: Request too large for model `qwen/qwen3.8-27b` in organization `org_1` service tier `on_demand` on input tokens per minute (ITPM): Limit 7000, Requested 7185, please reduce your message size and try again.";
    const OTPM: &str = "HTTP 429 Too Many Requests: Request too large for model `qwen/qwen3.8-27b` in organization `org_1` service tier `on_demand` on output tokens per minute (OTPM): Limit 1000, Requested 1404. The request's expected output tokens exceed the enforced limit; reduce max_tokens (or the request's expected output) and try again.";

    #[test]
    fn groq_refusals_of_a_request_too_large_name_the_ceiling() {
        assert_eq!(
            per_minute_refusal(TPM),
            Some(PerMinuteRefusal::Input {
                limit: 8_000,
                requested: 8_421
            })
        );
        assert_eq!(
            per_minute_refusal(ITPM),
            Some(PerMinuteRefusal::Input {
                limit: 7_000,
                requested: 7_185
            })
        );
        assert_eq!(
            per_minute_refusal(OTPM),
            Some(PerMinuteRefusal::Output {
                limit: 1_000,
                requested: 1_404
            })
        );
    }

    #[test]
    fn a_rate_limit_that_asks_for_a_wait_names_the_limit_but_refuses_nothing() {
        let wait = "HTTP 429 Too Many Requests: Rate limit reached for model `openai/gpt-oss-120b` in organization `org_1` service tier `on_demand` on tokens per minute (TPM): Limit 8000, Used 5410, Requested 5380. Please try again in 20.925s.";
        assert_eq!(per_minute_refusal(wait), None);
        assert_eq!(
            per_minute_limit(wait),
            Some(PerMinuteLimit {
                refusal: PerMinuteRefusal::Input {
                    limit: 8_000,
                    requested: 5_380
                },
                too_large: false
            })
        );
        assert!(per_minute_limit(TPM).unwrap().too_large);
        // OpenAI's words for the same limits.
        assert_eq!(
            per_minute_refusal(
                "Request too large for gpt-4o in organization org-1 on tokens per min (TPM): Limit 30000, Requested 50000."
            ),
            Some(PerMinuteRefusal::Input {
                limit: 30_000,
                requested: 50_000
            })
        );
        assert_eq!(
            per_minute_limit(
                "Rate limit reached for gpt-4o in organization org-1 on tokens per min (TPM): Limit 30000, Used 29000, Requested 2000. Please try again in 1.054s."
            )
            .map(|limit| limit.too_large),
            Some(false)
        );
        for message in [
            "HTTP 413: request too large",
            "Rate limit reached for gpt-4o on requests per min (RPM): Limit 500, Used 500, Requested 1.",
            "prompt is too long: 250000 tokens > 200000 maximum",
            "HTTP 429: rate limit exceeded",
        ] {
            assert_eq!(per_minute_limit(message), None, "{message}");
        }
    }

    #[test]
    fn a_ceiling_is_learned_once_per_model_and_only_lowered() {
        let ceilings = RequestCeilings::default();
        let refusal = per_minute_refusal(TPM).unwrap();
        assert!(ceilings.learn("groq", "openai/gpt-oss-120b", refusal));
        // The same refusal again: nothing more to fit the request to.
        assert!(!ceilings.learn("groq", "openai/gpt-oss-120b", refusal));
        assert_eq!(
            ceilings.get("groq", "openai/gpt-oss-120b"),
            Ceiling {
                input: Some(8_000),
                output: None
            }
        );
        // Another model, or the same one elsewhere, keeps its own.
        assert_eq!(
            ceilings.get("groq", "openai/gpt-oss-20b"),
            Ceiling::default()
        );
        assert_eq!(
            ceilings.get("openrouter", "openai/gpt-oss-120b"),
            Ceiling::default()
        );
        assert!(ceilings.learn(
            "groq",
            "openai/gpt-oss-120b",
            PerMinuteRefusal::Input {
                limit: 6_000,
                requested: 6_500
            }
        ));
        assert!(ceilings.learn(
            "groq",
            "qwen/qwen3.8-27b",
            per_minute_refusal(OTPM).unwrap()
        ));
        assert_eq!(
            ceilings.get("groq", "qwen/qwen3.8-27b"),
            Ceiling {
                input: None,
                output: Some(1_000)
            }
        );
        // A limit no coding request can fit under stays a refusal.
        assert!(!ceilings.learn(
            "groq",
            "openai/gpt-oss-safeguard-20b",
            PerMinuteRefusal::Input {
                limit: 2_000,
                requested: 5_261
            }
        ));
        assert_eq!(
            ceilings.get("groq", "openai/gpt-oss-safeguard-20b"),
            Ceiling::default()
        );
    }

    #[test]
    fn tools_are_sized_for_the_smaller_of_the_window_and_the_ceiling() {
        let ceiling = Ceiling {
            input: Some(8_000),
            output: None,
        };
        assert_eq!(ceiling.tool_window(Some(131_072)), Some(8_000));
        assert_eq!(ceiling.tool_window(None), Some(8_000));
        assert_eq!(ceiling.tool_window(Some(0)), Some(8_000));
        assert_eq!(ceiling.tool_window(Some(4_096)), Some(4_096));
        assert_eq!(Ceiling::default().tool_window(Some(131_072)), Some(131_072));
        assert_eq!(Ceiling::default().tool_window(None), None);
    }
}
