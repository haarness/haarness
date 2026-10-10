//! Embedded model metadata: context windows, output limits, and pricing.
//!
//! Curated from public provider documentation and models.dev at authoring
//! time. Deliberately static — a local-first binary must not need the network
//! to know a context window. `[models."provider/model"]` config overrides win
//! over this table (applied by the composition root). Prefix matching handles
//! dated snapshots such as `claude-sonnet-4-6-20260201`.

use haarness_protocol::{LongContextPricing, ModelInfo, ModelPricing};

pub struct CatalogEntry {
    pub prefix: &'static str,
    pub context_window: u64,
    pub max_output_tokens: u64,
    /// USD per million tokens: input, output, cache read, cache write.
    pub pricing: Option<(f64, f64, f64, f64)>,
}

const fn entry(
    prefix: &'static str,
    context_window: u64,
    max_output_tokens: u64,
    pricing: Option<(f64, f64, f64, f64)>,
) -> CatalogEntry {
    CatalogEntry {
        prefix,
        context_window,
        max_output_tokens,
        pricing,
    }
}

/// Longest-prefix table per provider. Order within a provider does not matter;
/// the longest matching prefix wins, so a variant priced differently from its
/// family (`-pro`, `-mini`, `-nano`, `-image`) needs its own entry.
///
/// Prices are the providers' list prices in USD per million tokens, checked
/// against OpenRouter's live catalogue on 2026-10-10 (it passes list prices
/// through) and the providers' own pricing pages where the two disagree.
/// Gemini's context-cache storage is billed per hour, not per written token, so
/// its cache-write price stays 0.
const CATALOG: &[(&str, &[CatalogEntry])] = &[
    (
        "openai",
        &[
            entry(
                "gpt-6.1-sol",
                1_050_000,
                128_000,
                Some((2.0, 10.0, 0.1, 2.5)),
            ),
            entry("gpt-6-sol", 1_050_000, 128_000, Some((2.0, 10.0, 0.2, 2.5))),
            entry(
                "gpt-6-luna",
                1_050_000,
                128_000,
                Some((0.1, 0.5, 0.01, 0.125)),
            ),
            entry(
                "gpt-6-astra",
                1_050_000,
                128_000,
                Some((10.0, 50.0, 1.0, 12.5)),
            ),
            entry(
                "gpt-5.6-terra",
                1_050_000,
                128_000,
                Some((2.0, 12.0, 0.2, 2.5)),
            ),
            entry(
                "gpt-5.6-luna",
                1_050_000,
                128_000,
                Some((0.2, 1.2, 0.02, 0.25)),
            ),
            // OpenAI's promotional GPT-5.6 Sol price (through at least
            // 2026-11-21); OpenRouter still lists $2/$10. "Pro" is
            // `reasoning.mode`, not a model, so there is no `-pro` entry.
            entry(
                "gpt-5.6-sol",
                1_050_000,
                128_000,
                Some((4.0, 20.0, 0.4, 5.0)),
            ),
            // `gpt-5.6` is the documented alias for GPT-5.6 Sol.
            entry("gpt-5.6", 1_050_000, 128_000, Some((4.0, 20.0, 0.4, 5.0))),
            entry(
                "gpt-5.5-pro",
                1_050_000,
                128_000,
                Some((30.0, 180.0, 0.0, 0.0)),
            ),
            entry("gpt-5.5", 1_050_000, 128_000, Some((5.0, 30.0, 0.5, 0.0))),
            entry(
                "gpt-chat-latest",
                400_000,
                128_000,
                Some((5.0, 30.0, 0.5, 0.0)),
            ),
            entry(
                "gpt-5.4-pro",
                1_050_000,
                128_000,
                Some((30.0, 180.0, 0.0, 0.0)),
            ),
            entry(
                "gpt-5.4-image",
                272_000,
                128_000,
                Some((8.0, 15.0, 2.0, 0.0)),
            ),
            entry(
                "gpt-5.4-mini",
                400_000,
                128_000,
                Some((0.75, 4.5, 0.075, 0.0)),
            ),
            entry(
                "gpt-5.4-nano",
                400_000,
                128_000,
                Some((0.2, 1.25, 0.02, 0.0)),
            ),
            entry("gpt-5.4", 1_050_000, 128_000, Some((2.5, 15.0, 0.25, 0.0))),
            entry(
                "gpt-5.3-codex",
                400_000,
                128_000,
                Some((1.75, 14.0, 0.175, 0.0)),
            ),
            entry(
                "gpt-5.2-pro",
                400_000,
                128_000,
                Some((21.0, 168.0, 0.0, 0.0)),
            ),
            entry("gpt-5.2", 400_000, 128_000, Some((1.75, 14.0, 0.175, 0.0))),
            entry(
                "gpt-5.1-codex-mini",
                400_000,
                128_000,
                Some((0.25, 2.0, 0.025, 0.0)),
            ),
            entry("gpt-5-pro", 400_000, 128_000, Some((15.0, 120.0, 0.0, 0.0))),
            entry(
                "gpt-5-image-mini",
                400_000,
                128_000,
                Some((2.5, 2.0, 0.25, 0.0)),
            ),
            entry(
                "gpt-5-image",
                400_000,
                128_000,
                Some((10.0, 10.0, 1.25, 0.0)),
            ),
            entry(
                "gpt-5-mini",
                400_000,
                128_000,
                Some((0.25, 2.0, 0.025, 0.0)),
            ),
            entry(
                "gpt-5-nano",
                400_000,
                128_000,
                Some((0.05, 0.4, 0.005, 0.0)),
            ),
            // GPT-5 and GPT-5.1 (and their Codex builds) share one price.
            entry("gpt-5", 400_000, 128_000, Some((1.25, 10.0, 0.125, 0.0))),
            entry(
                "gpt-4.1-mini",
                1_047_576,
                32_768,
                Some((0.4, 1.6, 0.1, 0.0)),
            ),
            entry(
                "gpt-4.1-nano",
                1_047_576,
                32_768,
                Some((0.1, 0.4, 0.025, 0.0)),
            ),
            entry("gpt-4.1", 1_047_576, 32_768, Some((2.0, 8.0, 0.5, 0.0))),
            entry(
                "gpt-4o-mini",
                128_000,
                16_384,
                Some((0.15, 0.6, 0.075, 0.0)),
            ),
            entry("gpt-4o", 128_000, 16_384, Some((2.5, 10.0, 1.25, 0.0))),
            entry("o3-pro", 200_000, 100_000, Some((20.0, 80.0, 0.0, 0.0))),
            entry("o3-mini", 200_000, 100_000, Some((1.1, 4.4, 0.55, 0.0))),
            entry("o3", 200_000, 100_000, Some((2.0, 8.0, 0.5, 0.0))),
            entry("o4-mini", 200_000, 100_000, Some((1.1, 4.4, 0.275, 0.0))),
            entry("o1-pro", 200_000, 100_000, Some((150.0, 600.0, 0.0, 0.0))),
            entry("o1", 200_000, 100_000, Some((15.0, 60.0, 7.5, 0.0))),
            entry("codex", 400_000, 128_000, Some((1.25, 10.0, 0.125, 0.0))),
        ],
    ),
    (
        "anthropic",
        &[
            // Limits verified through Anthropic's live Models API on 2026-09-23.
            // A newly discovered release never inherits its family's price: give
            // it an entry once its list price is published.
            entry(
                "claude-opus-5-5",
                1_000_000,
                128_000,
                Some((4.0, 20.0, 0.2, 5.0)),
            ),
            entry(
                "claude-sonnet-5-5",
                1_000_000,
                128_000,
                Some((2.0, 10.0, 0.1, 2.5)),
            ),
            // Prompts over 100K tokens cost 5x ($0.50/$2.50); this table
            // holds the short-context rate only.
            entry(
                "claude-haiku-5-5",
                1_000_000,
                128_000,
                Some((0.1, 0.5, 0.01, 0.125)),
            ),
            entry(
                "claude-fable-5-1",
                1_000_000,
                128_000,
                Some((10.0, 50.0, 0.25, 12.5)),
            ),
            entry(
                "claude-mythos-5-1",
                1_000_000,
                128_000,
                Some((10.0, 50.0, 0.25, 12.5)),
            ),
            entry(
                "claude-opus-4-8",
                1_000_000,
                128_000,
                Some((5.0, 25.0, 0.5, 6.25)),
            ),
            entry(
                "claude-opus-4-7",
                1_000_000,
                128_000,
                Some((5.0, 25.0, 0.5, 6.25)),
            ),
            entry(
                "claude-opus-4-6",
                1_000_000,
                64_000,
                Some((5.0, 25.0, 0.5, 6.25)),
            ),
            // Opus 4 and 4.1 kept the old Opus price; 4.5 onwards cost a third.
            entry(
                "claude-opus-4-1",
                200_000,
                32_000,
                Some((15.0, 75.0, 1.5, 18.75)),
            ),
            entry(
                "claude-opus-4-2025",
                200_000,
                32_000,
                Some((15.0, 75.0, 1.5, 18.75)),
            ),
            entry(
                "claude-opus-4",
                200_000,
                64_000,
                Some((5.0, 25.0, 0.5, 6.25)),
            ),
            entry(
                "claude-sonnet-4-6",
                1_000_000,
                64_000,
                Some((3.0, 15.0, 0.3, 3.75)),
            ),
            entry(
                "claude-sonnet-4",
                200_000,
                64_000,
                Some((3.0, 15.0, 0.3, 3.75)),
            ),
            entry(
                "claude-haiku-4",
                200_000,
                64_000,
                Some((1.0, 5.0, 0.1, 1.25)),
            ),
            entry(
                "claude-opus-5",
                1_000_000,
                128_000,
                Some((5.0, 25.0, 0.5, 6.25)),
            ),
            // Sonnet 5 and 5.5 share one price.
            entry(
                "claude-sonnet-5",
                1_000_000,
                128_000,
                Some((2.0, 10.0, 0.2, 2.5)),
            ),
            entry(
                "claude-fable-5",
                1_000_000,
                128_000,
                Some((10.0, 50.0, 1.0, 12.5)),
            ),
            entry(
                "claude-3-5-haiku",
                200_000,
                8_192,
                Some((0.8, 4.0, 0.08, 1.0)),
            ),
            entry("claude-3-5", 200_000, 8_192, Some((3.0, 15.0, 0.3, 3.75))),
        ],
    ),
    (
        "gemini",
        &[
            entry(
                "gemini-3.8-flash",
                1_048_576,
                65_536,
                Some((0.75, 3.75, 0.075, 0.0)),
            ),
            entry(
                "gemini-3.7-flash",
                1_048_576,
                65_536,
                Some((0.75, 3.75, 0.075, 0.0)),
            ),
            entry(
                "gemini-3.6-flash",
                1_048_576,
                65_536,
                Some((0.75, 3.75, 0.075, 0.0)),
            ),
            entry(
                "gemini-3.5-flash-lite",
                1_048_576,
                65_536,
                Some((0.3, 2.5, 0.03, 0.0)),
            ),
            entry(
                "gemini-3.5-flash",
                1_048_576,
                65_536,
                Some((1.5, 9.0, 0.15, 0.0)),
            ),
            // Image generation and editing; never offered for chat.
            entry(
                "gemini-nano-banana",
                131_072,
                32_768,
                Some((1.5, 7.5, 0.0, 0.0)),
            ),
            entry(
                "gemini-3.1-flash-lite-image",
                65_536,
                58_982,
                Some((0.25, 1.5, 0.0, 0.0)),
            ),
            entry(
                "gemini-3.1-flash-lite",
                1_048_576,
                65_536,
                Some((0.25, 1.5, 0.025, 0.0)),
            ),
            entry(
                "gemini-3.1-flash-image",
                131_072,
                32_768,
                Some((0.5, 3.0, 0.0, 0.0)),
            ),
            entry(
                "gemini-3.1-pro",
                1_048_576,
                65_536,
                Some((2.0, 12.0, 0.2, 0.0)),
            ),
            entry(
                "gemini-3-pro-image",
                131_072,
                32_768,
                Some((2.0, 12.0, 0.2, 0.0)),
            ),
            entry(
                "gemini-3-flash",
                1_048_576,
                65_536,
                Some((0.5, 3.0, 0.05, 0.0)),
            ),
            entry(
                "gemini-2.5-pro",
                1_048_576,
                65_536,
                Some((1.25, 10.0, 0.125, 0.0)),
            ),
            entry(
                "gemini-2.5-flash-lite",
                1_048_576,
                65_536,
                Some((0.1, 0.4, 0.01, 0.0)),
            ),
            entry(
                "gemini-2.5-flash",
                1_048_576,
                65_536,
                Some((0.3, 2.5, 0.03, 0.0)),
            ),
            entry("gemini", 1_048_576, 65_536, None),
        ],
    ),
];

/// Whole-request rates once a prompt passes a size, keyed by the catalog
/// entry's prefix: (provider, prefix, above input tokens, input, output, cache
/// read, cache write). From the providers' price pages on 2026-10-10.
const LONG_CONTEXT: &[(&str, &str, u64, f64, f64, f64, f64)] = &[
    ("openai", "gpt-6-astra", 272_000, 20.0, 75.0, 2.0, 25.0),
    ("openai", "gpt-6.1-sol", 272_000, 4.0, 15.0, 0.2, 5.0),
    ("openai", "gpt-6-sol", 272_000, 4.0, 15.0, 0.4, 5.0),
    ("openai", "gpt-6-luna", 272_000, 0.2, 0.75, 0.02, 0.25),
    ("openai", "gpt-5.6-sol", 272_000, 8.0, 30.0, 0.8, 10.0),
    ("openai", "gpt-5.6", 272_000, 8.0, 30.0, 0.8, 10.0),
    ("openai", "gpt-5.6-terra", 272_000, 4.0, 18.0, 0.4, 5.0),
    ("openai", "gpt-5.6-luna", 272_000, 0.4, 1.8, 0.04, 0.5),
    ("openai", "gpt-5.5-pro", 272_000, 60.0, 270.0, 0.0, 0.0),
    ("openai", "gpt-5.5", 272_000, 10.0, 45.0, 1.0, 0.0),
    ("openai", "gpt-5.4-pro", 272_000, 60.0, 270.0, 0.0, 0.0),
    ("openai", "gpt-5.4", 272_000, 5.0, 22.5, 0.5, 0.0),
    (
        "anthropic",
        "claude-haiku-5-5",
        100_000,
        0.5,
        2.5,
        0.05,
        0.625,
    ),
    ("gemini", "gemini-3.1-pro", 200_000, 4.0, 18.0, 0.4, 0.0),
    ("gemini", "gemini-2.5-pro", 200_000, 2.5, 15.0, 0.25, 0.0),
];

fn long_context(provider: &str, prefix: &str) -> Option<LongContextPricing> {
    LONG_CONTEXT
        .iter()
        .find(|entry| entry.0 == provider && entry.1 == prefix)
        .map(
            |&(_, _, above_input_tokens, input, output, cache_read, cache_write)| {
                LongContextPricing {
                    above_input_tokens,
                    input,
                    output,
                    cache_read,
                    cache_write,
                }
            },
        )
}

#[must_use]
pub fn lookup(provider: &str, model_id: &str) -> Option<&'static CatalogEntry> {
    let entries = CATALOG
        .iter()
        .find(|(candidate, _)| *candidate == provider)?
        .1;
    // Routers and aggregators spell Claude versions with dots
    // (`claude-opus-5.5`); Anthropic's own ids use dashes (`claude-opus-5-5`).
    let model_id = if provider == "anthropic" {
        model_id.replace('.', "-")
    } else {
        model_id.to_owned()
    };
    entries
        .iter()
        .filter(|entry| model_id.starts_with(entry.prefix))
        .max_by_key(|entry| entry.prefix.len())
}

/// Fill catalog metadata into a `ModelInfo`, leaving explicitly set values
/// untouched.
#[must_use]
pub fn annotate(provider: &str, mut info: ModelInfo) -> ModelInfo {
    info.display_name = haarness_protocol::model_display_name(&info.id, Some(&info.display_name));
    let canonical = info
        .id
        .split_once('/')
        .map_or(info.id.as_str(), |(_, id)| id);
    let metadata = lookup(provider, &info.id)
        .map(|entry| (provider, entry))
        .or_else(|| {
            CATALOG.iter().find_map(|(provider, _)| {
                lookup(provider, canonical).map(|entry| (*provider, entry))
            })
        });
    if let Some((family, entry)) = metadata {
        info.context_window.get_or_insert(entry.context_window);
        info.max_output_tokens
            .get_or_insert(entry.max_output_tokens);
        if info.pricing.is_none() {
            info.pricing = entry
                .pricing
                .map(|(input, output, cache_read, cache_write)| ModelPricing {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    long_context: long_context(family, entry.prefix),
                });
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::{annotate, lookup};

    #[test]
    fn longest_prefix_wins_and_dated_snapshots_match() {
        let plain = lookup("openai", "gpt-5.4").unwrap();
        assert!((plain.pricing.unwrap().0 - 2.5).abs() < f64::EPSILON);
        let mini = lookup("openai", "gpt-5.4-mini-2026-01-01").unwrap();
        assert!((mini.pricing.unwrap().0 - 0.75).abs() < f64::EPSILON);
        assert_eq!(
            lookup("anthropic", "claude-sonnet-4-6-20260201")
                .unwrap()
                .context_window,
            1_000_000
        );
        assert!(lookup("openai", "unknown-model").is_none());
        // A `-pro` build costs far more than its family: it never borrows the family's price.
        let pro = lookup("openai", "gpt-5.5-pro").unwrap().pricing.unwrap();
        assert!((pro.0 - 30.0).abs() < f64::EPSILON && (pro.1 - 180.0).abs() < f64::EPSILON);
        // Opus 4.1 kept the old Opus price; the bare `claude-opus-4` prefix is Opus 4.5's.
        assert!(
            (lookup("anthropic", "claude-opus-4-1-20250805")
                .unwrap()
                .pricing
                .unwrap()
                .0
                - 15.0)
                .abs()
                < f64::EPSILON
        );
        assert!(
            (lookup("anthropic", "claude-opus-4-5")
                .unwrap()
                .pricing
                .unwrap()
                .0
                - 5.0)
                .abs()
                < f64::EPSILON
        );
        assert!(lookup("unknown-provider", "gpt-5.4").is_none());
    }

    #[test]
    fn current_flagships_are_priced() {
        for (provider, model, input, output) in [
            ("anthropic", "claude-opus-5-5", 4.0, 20.0),
            ("anthropic", "claude-sonnet-5-5", 2.0, 10.0),
            ("anthropic", "claude-haiku-5-5", 0.1, 0.5),
            ("openai", "gpt-5.6-sol", 4.0, 20.0),
            ("anthropic", "claude-fable-5-1", 10.0, 50.0),
            ("openai", "gpt-6.1-sol", 2.0, 10.0),
            ("openai", "gpt-6-sol", 2.0, 10.0),
            ("openai", "gpt-5.5", 5.0, 30.0),
            ("gemini", "gemini-3.8-flash", 0.75, 3.75),
            ("gemini", "gemini-3.1-pro-preview", 2.0, 12.0),
        ] {
            let pricing = lookup(provider, model)
                .and_then(|entry| entry.pricing)
                .unwrap_or_else(|| panic!("{provider}/{model} has no price"));
            assert!(
                (pricing.0 - input).abs() < f64::EPSILON
                    && (pricing.1 - output).abs() < f64::EPSILON,
                "{provider}/{model}: {pricing:?}"
            );
        }
    }

    #[test]
    fn long_prompts_carry_their_tier() {
        let info = |id: &str| haarness_protocol::ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: true,
            reasoning_efforts: Vec::new(),
            images: true,
            video: false,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        };
        let haiku = annotate("anthropic", info("claude-haiku-5-5"))
            .pricing
            .unwrap();
        let tier = haiku.long_context.unwrap();
        assert_eq!(tier.above_input_tokens, 100_000);
        assert!(
            (tier.input - 0.5).abs() < f64::EPSILON && (tier.output - 2.5).abs() < f64::EPSILON
        );
        // Routed ids find the native tier too.
        let routed = annotate("router", info("openai/gpt-6.1-sol"))
            .pricing
            .unwrap();
        assert_eq!(routed.long_context.unwrap().above_input_tokens, 272_000);
        let sonnet = annotate("anthropic", info("claude-sonnet-5-5"))
            .pricing
            .unwrap();
        assert!(sonnet.long_context.is_none());
        // Every tier belongs to an existing entry.
        for (provider, prefix, ..) in super::LONG_CONTEXT {
            assert_eq!(lookup(provider, prefix).unwrap().prefix, *prefix);
        }
    }

    #[test]
    fn router_spellings_of_claude_versions_find_the_native_entry() {
        // Cloud sessions name models the OpenRouter way: dots, not dashes.
        let dotted = lookup("anthropic", "claude-opus-5.5").unwrap();
        assert_eq!(dotted.prefix, "claude-opus-5-5");
        let info = haarness_protocol::ModelInfo {
            id: "anthropic/claude-opus-5.5".into(),
            display_name: "Claude Opus 5.5".into(),
            reasoning: true,
            reasoning_efforts: Vec::new(),
            images: true,
            video: false,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        };
        let pricing = annotate("router", info).pricing.unwrap();
        assert!((pricing.input - 4.0).abs() < f64::EPSILON, "{pricing:?}");
    }

    #[test]
    fn annotate_fills_missing_metadata_only() {
        let info = haarness_protocol::ModelInfo {
            id: "claude-sonnet-4-6".into(),
            display_name: "Sonnet".into(),
            reasoning: true,
            reasoning_efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
            images: true,
            video: false,
            tools: true,
            context_window: Some(12345),
            max_output_tokens: None,
            pricing: None,
        };
        let annotated = annotate("anthropic", info);
        assert_eq!(annotated.context_window, Some(12345), "explicit value kept");
        assert_eq!(annotated.max_output_tokens, Some(64_000));
        let pricing = annotated.pricing.unwrap();
        assert!((pricing.input - 3.0).abs() < f64::EPSILON);
        assert!((pricing.cache_write - 3.75).abs() < f64::EPSILON);
    }
}
