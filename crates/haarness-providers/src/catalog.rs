//! Embedded model metadata: context windows, output limits, and pricing.
//!
//! Curated from public provider documentation and models.dev at authoring
//! time. Deliberately static — a local-first binary must not need the network
//! to know a context window. `[models."provider/model"]` config overrides win
//! over this table (applied by the composition root). Prefix matching handles
//! dated snapshots such as `claude-sonnet-4-6-20260201`.

use haarness_protocol::{ModelInfo, ModelPricing};

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
/// the longest matching prefix wins.
const CATALOG: &[(&str, &[CatalogEntry])] = &[
    (
        "openai",
        &[
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
                Some((2.0, 12.0, 0.2, 0.0)),
            ),
            entry(
                "gpt-5.6-luna",
                1_050_000,
                128_000,
                Some((0.2, 1.2, 0.02, 0.0)),
            ),
            entry(
                "gpt-5.6-sol",
                1_050_000,
                128_000,
                Some((4.0, 20.0, 0.4, 0.0)),
            ),
            // `gpt-5.6` is the documented alias for GPT-5.6 Sol.
            entry("gpt-5.6", 1_050_000, 128_000, Some((4.0, 20.0, 0.4, 0.0))),
            // Previous-generation model still served by the API and ChatGPT;
            // its public list price is not published in the sources above.
            entry("gpt-5.5", 400_000, 128_000, None),
            entry(
                "gpt-5.4-mini",
                400_000,
                128_000,
                Some((0.25, 2.0, 0.025, 0.0)),
            ),
            entry(
                "gpt-5.4-nano",
                400_000,
                128_000,
                Some((0.05, 0.4, 0.005, 0.0)),
            ),
            entry("gpt-5.4", 400_000, 128_000, Some((1.25, 10.0, 0.125, 0.0))),
            entry("gpt-5", 400_000, 128_000, Some((1.25, 10.0, 0.125, 0.0))),
            entry("gpt-4.1", 1_047_576, 32_768, Some((2.0, 8.0, 0.5, 0.0))),
            entry("gpt-4o", 128_000, 16_384, Some((2.5, 10.0, 1.25, 0.0))),
            entry("o3", 200_000, 100_000, Some((2.0, 8.0, 0.5, 0.0))),
            entry("o4-mini", 200_000, 100_000, Some((1.1, 4.4, 0.275, 0.0))),
            entry("codex", 400_000, 128_000, Some((1.25, 10.0, 0.125, 0.0))),
        ],
    ),
    (
        "anthropic",
        &[
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
            entry("claude-3-5", 200_000, 8_192, Some((3.0, 15.0, 0.3, 3.75))),
        ],
    ),
    (
        "gemini",
        &[
            entry("gemini-3.7-flash", 1_048_576, 65_536, None),
            entry("gemini-3.6-flash", 1_048_576, 65_536, None),
            entry("gemini-3.5-flash-lite", 1_048_576, 65_536, None),
            entry("gemini-3.5-flash", 1_048_576, 65_536, None),
            entry("gemini-3.1-flash-lite", 1_048_576, 65_536, None),
            entry(
                "gemini-3.1-pro",
                1_048_576,
                65_536,
                Some((1.25, 10.0, 0.31, 0.0)),
            ),
            entry(
                "gemini-3-flash",
                1_048_576,
                65_536,
                Some((0.1, 0.4, 0.025, 0.0)),
            ),
            entry(
                "gemini-2.5-pro",
                1_048_576,
                65_536,
                Some((1.25, 10.0, 0.31, 0.0)),
            ),
            entry(
                "gemini-2.5-flash",
                1_048_576,
                65_536,
                Some((0.3, 2.5, 0.075, 0.0)),
            ),
            entry("gemini", 1_048_576, 65_536, None),
        ],
    ),
];

#[must_use]
pub fn lookup(provider: &str, model_id: &str) -> Option<&'static CatalogEntry> {
    let entries = CATALOG
        .iter()
        .find(|(candidate, _)| *candidate == provider)?
        .1;
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
    let metadata = lookup(provider, &info.id).or_else(|| {
        CATALOG
            .iter()
            .find_map(|(provider, _)| lookup(provider, canonical))
    });
    if let Some(entry) = metadata {
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
        assert!((plain.pricing.unwrap().0 - 1.25).abs() < f64::EPSILON);
        let mini = lookup("openai", "gpt-5.4-mini-2026-01-01").unwrap();
        assert!((mini.pricing.unwrap().0 - 0.25).abs() < f64::EPSILON);
        assert_eq!(
            lookup("anthropic", "claude-sonnet-4-6-20260201")
                .unwrap()
                .context_window,
            1_000_000
        );
        assert!(lookup("openai", "unknown-model").is_none());
        assert!(lookup("unknown-provider", "gpt-5.4").is_none());
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
