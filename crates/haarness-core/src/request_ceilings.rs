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
//!
//! The same goes for a model's context window where its metadata states none
//! (a server a person set up, whose window Haarness otherwise takes as 128K)
//! or a larger one than the server runs it with: a refusal of a request as
//! longer than the model takes shows it (see [`shown_window`]).

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

/// The smallest context window learned from a refusal: below this no coding
/// request fits whatever is compacted, and the refusal stands.
const LEAST_WINDOW: u64 = 4_096;

/// The context window a provider's refusal of a request as too long shows,
/// given the size of the refused request as Haarness measured it
/// (`refused`): the window the refusal names, or, where Haarness counts
/// fewer tokens than the server does, no more than the refused request held,
/// so the next request is measured against what the server took. A named
/// window far above the refused request is about the room left for the reply
/// (vLLM's "'max_tokens' is too large … maximum context length is 32768
/// tokens and your request has 1200 input tokens"), not the history: it is
/// taken as named.
pub(crate) fn shown_window(message: &str, refused: u64) -> Option<u64> {
    let window = match named_window(message) {
        Some(named) if refused >= named / 2 => named.min(refused),
        Some(named) => named,
        None => refused,
    };
    (window >= LEAST_WINDOW).then_some(window)
}

/// The window a refusal of a request as too long names, in the words of the
/// servers people run or reach: vLLM, SGLang, OpenAI, DeepSeek and
/// OpenRouter ("maximum context length is 32768 tokens"), llama.cpp ("exceeds
/// the available context size (16384 tokens)"), LM Studio ("loaded with
/// context length of only 4096 tokens"), Moonshot ("exceeded model token
/// limit: 131072"), Text Generation Inference ("`inputs` tokens +
/// `max_new_tokens` must be <= 32769"), Mistral ("too large for model with
/// 32768 maximum context length"), xAI ("maximum prompt length is 131072"),
/// Qwen on DashScope ("Range of input length should be [1, 129024]"),
/// DeepInfra ("exceeds maximum input length 131071"), Gemini ("exceeds the
/// maximum number of tokens allowed (1048576)") and Claude ("prompt is too
/// long: 210000 tokens > 200000 maximum").
fn named_window(message: &str) -> Option<u64> {
    const BEFORE: [&str; 17] = [
        "maximum context length is ",
        // DeepInfra: "should not exceed max_num_tokens (32768)".
        "max_num_tokens (",
        // OpenRouter: "longer than the model's max input length (131072 tokens)".
        "max input length (",
        "maximum context length: ",
        "available context size (",
        "context length of only ",
        "context length is only ",
        "configured context size is ",
        "model token limit: ",
        "must be <= ",
        "too large for model with ",
        "maximum prompt length is ",
        "input length should be [1, ",
        "maximum input length ",
        "maximum allowed input length of ",
        "maximum number of tokens allowed (",
        " tokens > ",
    ];
    let lower = message.to_ascii_lowercase();
    BEFORE
        .iter()
        .filter_map(|before| {
            let (_, rest) = lower.split_once(before)?;
            let digits: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .filter(char::is_ascii_digit)
                .collect();
            digits.parse::<u64>().ok().filter(|window| *window > 0)
        })
        .min()
}

/// The ceilings learned for one model on one provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Ceiling {
    /// The most prompt tokens one request may carry.
    pub input: Option<u64>,
    /// The most output tokens one reply may be asked for.
    pub output: Option<u64>,
    /// The context window a refusal of a request as too long showed.
    pub window: Option<u64>,
}

impl Ceiling {
    /// The window the tool policy sizes this model's tools for: its context
    /// window, or the account's ceiling where that is smaller.
    pub fn tool_window(self, context_window: Option<u64>) -> Option<u64> {
        match (self.context_window(context_window), self.input) {
            (Some(window), Some(ceiling)) => Some(window.min(ceiling)),
            (None, Some(ceiling)) => Some(ceiling),
            (window, None) => window,
        }
    }

    /// The model's context window: as its metadata states it
    /// (`stated`), or the smaller one a refusal showed.
    pub fn context_window(self, stated: Option<u64>) -> Option<u64> {
        match (stated.filter(|window| *window > 0), self.window) {
            (Some(stated), Some(shown)) => Some(stated.min(shown)),
            (stated, shown) => stated.or(shown),
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

    /// Learns that `model` on `provider` takes no more than `window` tokens
    /// of context ([`shown_window`]). True when that is less than it knew.
    pub fn learn_window(&self, provider: &str, model: &str, window: u64) -> bool {
        if window < LEAST_WINDOW {
            return false;
        }
        let mut ceiling = self
            .learned
            .entry((provider.to_owned(), model.to_owned()))
            .or_default();
        if ceiling.window.is_some_and(|known| known <= window) {
            return false;
        }
        ceiling.window = Some(window);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Ceiling, PerMinuteLimit, PerMinuteRefusal, RequestCeilings, per_minute_limit,
        per_minute_refusal, shown_window,
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
                ..Ceiling::default()
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
                output: Some(1_000),
                ..Ceiling::default()
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
            ..Ceiling::default()
        };
        assert_eq!(ceiling.tool_window(Some(131_072)), Some(8_000));
        assert_eq!(ceiling.tool_window(None), Some(8_000));
        assert_eq!(ceiling.tool_window(Some(0)), Some(8_000));
        assert_eq!(ceiling.tool_window(Some(4_096)), Some(4_096));
        assert_eq!(Ceiling::default().tool_window(Some(131_072)), Some(131_072));
        assert_eq!(Ceiling::default().tool_window(None), None);
    }

    #[test]
    fn a_refusal_as_too_long_shows_the_window_the_server_runs() {
        // Each server's words, against a refused request Haarness measured
        // close to what the server counted.
        for (message, window) in [
            (
                "HTTP 400 Bad Request: This model's maximum context length is 32768 tokens. However, you requested 33905 tokens in the messages, Please reduce the length of the messages.",
                32_768,
            ),
            (
                "HTTP 400 Bad Request: This endpoint's maximum context length is 131,072 tokens. However, you requested about 140210 tokens",
                131_072,
            ),
            (
                "HTTP 400 Bad Request: request (34000 tokens) exceeds the available context size (32768 tokens), try increasing it",
                32_768,
            ),
            (
                "Trying to keep the first 5000 tokens when context the overflows. However, the model is loaded with context length of only 32768 tokens, which is not enough.",
                32_768,
            ),
            (
                "HTTP 400 Bad Request: Invalid request: Your request exceeded model token limit: 32768",
                32_768,
            ),
            (
                "HTTP 422 Unprocessable Entity: Input validation error: `inputs` tokens + `max_new_tokens` must be <= 32769. Given: 33000 `inputs` tokens and 16 `max_new_tokens`",
                32_769,
            ),
            (
                "Prompt contains 34000 tokens and 0 draft tokens, too large for model with 32768 maximum context length",
                32_768,
            ),
            (
                "HTTP 400 Bad Request: This model's maximum prompt length is 32768 but the request contains 34000 tokens.",
                32_768,
            ),
            (
                "HTTP 400 Bad Request: <400> InternalError.Algo.InvalidParameter: Range of input length should be [1, 32768]",
                32_768,
            ),
            (
                "HTTP 400 Bad Request: Requested input length 34000 exceeds maximum input length 32768",
                32_768,
            ),
            (
                "The input token count (34000) exceeds the maximum number of tokens allowed (32768).",
                32_768,
            ),
            ("prompt is too long: 34000 tokens > 32768 maximum", 32_768),
            // DeepInfra's own servers, behind OpenRouter: the prompt length
            // is the refused request, not the window.
            (
                "HTTP 400: DeepInfra: The sum of prompt length (34000.0), query length (0) should not exceed max_num_tokens (32768)",
                32_768,
            ),
            // OpenRouter past every endpoint's input limit.
            (
                "HTTP 400: The input (34000 tokens) is longer than the model's max input length (32768 tokens).",
                32_768,
            ),
            // A Haarness router's words, naming the provider's window.
            (
                "HTTP 400 Bad Request: [upstream_400] The conversation is longer than this model's maximum context length, so the provider refused it (its maximum context length is 32768 tokens). Compact the conversation, or choose a model with a larger context window.",
                32_768,
            ),
            (
                "HTTP 400 Bad Request: The decoder prompt (length 34000) is longer than the maximum model length of 32768.",
                34_000,
            ),
        ] {
            assert_eq!(shown_window(message, 34_000), Some(window), "{message}");
        }
        // Words that name no window: no more than the refused request held.
        assert_eq!(
            shown_window(
                "HTTP 400 Bad Request: [1261] Prompt exceeds max length",
                20_000
            ),
            Some(20_000)
        );
        // Haarness counted fewer tokens than the server: the next request is
        // measured against what the server took.
        assert_eq!(
            shown_window(
                "This model's maximum context length is 32768 tokens. However, you requested 33905 tokens",
                26_000
            ),
            Some(26_000)
        );
        // A window far above the refused request is about the room for the
        // reply, not the history.
        assert_eq!(
            shown_window(
                "'max_tokens' or 'max_completion_tokens' is too large: 32000. This model's maximum context length is 32768 tokens and your request has 1200 input tokens",
                1_500
            ),
            Some(32_768)
        );
        // Below any window a coding request fits in.
        assert_eq!(
            shown_window("This model's maximum context length is 2048 tokens.", 2_500),
            None
        );
        assert_eq!(shown_window("Prompt exceeds max length", 3_000), None);
    }

    #[test]
    fn a_shown_window_is_kept_per_model_and_only_lowered() {
        let ceilings = RequestCeilings::default();
        assert!(ceilings.learn_window("selfhosted", "coder", 32_768));
        assert!(!ceilings.learn_window("selfhosted", "coder", 40_000));
        assert!(ceilings.learn_window("selfhosted", "coder", 30_000));
        assert!(!ceilings.learn_window("selfhosted", "coder", 1_000));
        let ceiling = ceilings.get("selfhosted", "coder");
        assert_eq!(ceiling.window, Some(30_000));
        assert_eq!(ceilings.get("selfhosted", "other").window, None);
        // The model's metadata states none, or a larger one: the shown one.
        assert_eq!(ceiling.context_window(None), Some(30_000));
        assert_eq!(ceiling.context_window(Some(131_072)), Some(30_000));
        assert_eq!(ceiling.context_window(Some(16_384)), Some(16_384));
        assert_eq!(ceiling.tool_window(None), Some(30_000));
        assert_eq!(Ceiling::default().context_window(Some(0)), None);
    }
}
