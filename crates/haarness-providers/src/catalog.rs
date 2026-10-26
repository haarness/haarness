//! Embedded model metadata: context windows, output limits, and pricing.
//!
//! Curated from public provider documentation and models.dev at authoring
//! time. Deliberately static — a local-first binary must not need the network
//! to know a context window. `[models."provider/model"]` config overrides win
//! over this table (applied by the composition root). Prefix matching handles
//! dated snapshots such as `claude-sonnet-4-6-20260201`.
//!
//! Prices come first from `model-prices.json`, Haarness's reviewed price
//! table: provider list prices checked by hand against each provider's price
//! page, the same file Haarness Cloud bills from, including the price changes
//! a provider has announced for a date. The catalog's own prices are the
//! fallback for models the table does not list.

use std::sync::LazyLock;

use chrono::NaiveDate;
use haarness_protocol::{LongContextPricing, ModelInfo, ModelPricing};
use serde::{Deserialize, Deserializer};

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
/// Prices here are fallbacks for models the reviewed table does not list: the
/// providers' list prices in USD per million tokens as OpenRouter's live
/// catalogue gave them on 2026-10-10 (it passes list prices through). A model
/// the table lists has no price here. Gemini's context-cache storage is billed
/// per hour, not per written token, so its cache-write price stays 0.
const CATALOG: &[(&str, &[CatalogEntry])] = &[
    (
        "openai",
        &[
            entry("gpt-6.1-sol", 1_050_000, 128_000, None),
            entry("gpt-6-sol", 1_050_000, 128_000, None),
            entry("gpt-6-luna", 1_050_000, 128_000, None),
            entry("gpt-6-astra", 1_050_000, 128_000, None),
            entry("gpt-5.6-terra", 1_050_000, 128_000, None),
            entry("gpt-5.6-luna", 1_050_000, 128_000, None),
            entry("gpt-5.6-sol", 1_050_000, 128_000, None),
            entry("gpt-5.6", 1_050_000, 128_000, None),
            entry(
                "gpt-5.5-pro",
                1_050_000,
                128_000,
                Some((30.0, 180.0, 0.0, 0.0)),
            ),
            entry("gpt-5.5", 1_050_000, 128_000, None),
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
            // A newly discovered release never inherits its family's price: list
            // it in the price table once its list price is published.
            entry("claude-opus-5-5", 1_000_000, 128_000, None),
            entry("claude-sonnet-5-5", 1_000_000, 128_000, None),
            entry("claude-haiku-5-5", 1_000_000, 128_000, None),
            entry("claude-fable-5-1", 1_000_000, 128_000, None),
            entry("claude-mythos-5-1", 1_000_000, 128_000, None),
            entry("claude-opus-4-8", 1_000_000, 128_000, None),
            entry("claude-opus-4-7", 1_000_000, 128_000, None),
            entry("claude-opus-4-6", 1_000_000, 64_000, None),
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
            entry("claude-sonnet-4-6", 1_000_000, 64_000, None),
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
            entry("claude-opus-5", 1_000_000, 128_000, None),
            entry("claude-sonnet-5", 1_000_000, 128_000, None),
            entry("claude-fable-5", 1_000_000, 128_000, None),
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
            // Probed 2026-10-15: the Models API lists 32,768 output tokens.
            entry("gemini-4-argon", 1_048_576, 32_768, None),
            entry("gemini-3.8-flash", 1_048_576, 65_536, None),
            entry(
                "gemini-3.7-flash",
                1_048_576,
                65_536,
                Some((0.75, 3.75, 0.075, 0.0)),
            ),
            entry("gemini-3.6-flash", 1_048_576, 65_536, None),
            entry("gemini-3.5-flash-lite", 1_048_576, 65_536, None),
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
            entry("gemini-3.1-flash-lite", 1_048_576, 65_536, None),
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
            entry("gemini-2.5-pro", 1_048_576, 65_536, None),
            entry("gemini-2.5-flash-lite", 1_048_576, 65_536, None),
            entry("gemini-2.5-flash", 1_048_576, 65_536, None),
            entry("gemini", 1_048_576, 65_536, None),
        ],
    ),
];

/// Whole-request rates once a prompt passes a size for fallback-priced
/// models, keyed by the catalog entry's prefix: (provider, prefix, above input
/// tokens, input, output, cache read, cache write). From the providers' price
/// pages on 2026-10-10.
const LONG_CONTEXT: &[(&str, &str, u64, f64, f64, f64, f64)] = &[
    ("openai", "gpt-5.5-pro", 272_000, 60.0, 270.0, 0.0, 0.0),
    ("openai", "gpt-5.4-pro", 272_000, 60.0, 270.0, 0.0, 0.0),
    ("openai", "gpt-5.4", 272_000, 5.0, 22.5, 0.5, 0.0),
    ("gemini", "gemini-3.1-pro", 200_000, 4.0, 18.0, 0.4, 0.0),
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

/// The reviewed price table, shared byte for byte with Haarness Cloud's
/// `config/model-prices.json`. `scripts/check-model-prices.py --sync` copies
/// it over; a test fails while the two differ.
const PRICE_TABLE: &str = include_str!("model-prices.json");

static REVIEWED: LazyLock<Vec<ReviewedPrice>> = LazyLock::new(|| {
    serde_json::from_str::<PriceTable>(PRICE_TABLE)
        .expect("valid embedded price table")
        .models
});

#[derive(Deserialize)]
struct PriceTable {
    models: Vec<ReviewedPrice>,
}

/// One reviewed model: `openai/gpt-6.1-sol`, with rates as decimal strings.
#[derive(Deserialize)]
struct ReviewedPrice {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    /// The rates until the first scheduled change.
    #[serde(flatten)]
    rates: ReviewedRates,
    /// Announced price changes in date order.
    #[serde(default)]
    changes: Vec<ReviewedChange>,
}

#[derive(Deserialize)]
struct ReviewedRates {
    #[serde(deserialize_with = "rate")]
    input: f64,
    #[serde(deserialize_with = "rate")]
    output: f64,
    #[serde(deserialize_with = "rate")]
    cache_read: f64,
    #[serde(deserialize_with = "rate")]
    cache_write: f64,
    #[serde(default)]
    long_context: Option<ReviewedTier>,
}

/// Every rate from 00:00 UTC on `from`, as Cloud bills it; a tier the change
/// does not list is gone from that day.
#[derive(Deserialize)]
struct ReviewedChange {
    from: NaiveDate,
    #[serde(flatten)]
    rates: ReviewedRates,
}

impl ReviewedPrice {
    /// The rates in force on `day` (UTC): the latest change that has come,
    /// else the entry's own.
    fn rates_on(&self, day: NaiveDate) -> &ReviewedRates {
        self.changes
            .iter()
            .filter(|change| change.from <= day)
            .max_by_key(|change| change.from)
            .map_or(&self.rates, |change| &change.rates)
    }
}

#[derive(Deserialize)]
struct ReviewedTier {
    above_input_tokens: u64,
    #[serde(deserialize_with = "rate")]
    input: f64,
    #[serde(deserialize_with = "rate")]
    output: f64,
    #[serde(deserialize_with = "rate")]
    cache_read: f64,
    #[serde(deserialize_with = "rate")]
    cache_write: f64,
}

fn rate<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(serde::de::Error::custom)
}

/// `anthropic/claude-haiku-5.5`, `claude-haiku-5.5` and `claude-haiku-5-5`
/// name one model: the vendor is dropped and dots read as dashes.
fn price_key(id: &str) -> String {
    id.rsplit_once('/')
        .map_or(id, |(_, model)| model)
        .to_ascii_lowercase()
        .replace('.', "-")
}

/// A dated snapshot of a listed model: `-20251001` or `-2026-01-01`.
fn is_snapshot(suffix: &str) -> bool {
    let Some(date) = suffix.strip_prefix('-') else {
        return false;
    };
    let digits = |part: &str, len: usize| {
        part.len() == len && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    digits(date, 8)
        || matches!(date.split('-').collect::<Vec<_>>()[..], [year, month, day]
            if digits(year, 4) && digits(month, 2) && digits(day, 2))
}

fn reviewed(model_id: &str) -> Option<&'static ReviewedPrice> {
    let key = price_key(model_id);
    REVIEWED.iter().find(|price| {
        std::iter::once(&price.id).chain(&price.aliases).any(|id| {
            let id = price_key(id);
            key == id || key.strip_prefix(&id).is_some_and(is_snapshot)
        })
    })
}

/// A model's list price today (UTC): the reviewed table's when it lists the
/// model (by id, alias or dated snapshot, from any provider), else the
/// catalog's fallback.
#[must_use]
pub fn pricing(provider: &str, model_id: &str) -> Option<ModelPricing> {
    pricing_on(provider, model_id, chrono::Utc::now().date_naive())
}

/// `pricing` on `day` (UTC): a scheduled change applies from its `from` date,
/// as in Haarness Cloud's billing.
fn pricing_on(provider: &str, model_id: &str, day: NaiveDate) -> Option<ModelPricing> {
    if let Some(price) = reviewed(model_id) {
        let rates = price.rates_on(day);
        return Some(ModelPricing {
            input: rates.input,
            output: rates.output,
            cache_read: rates.cache_read,
            cache_write: rates.cache_write,
            long_context: rates.long_context.as_ref().map(|tier| LongContextPricing {
                above_input_tokens: tier.above_input_tokens,
                input: tier.input,
                output: tier.output,
                cache_read: tier.cache_read,
                cache_write: tier.cache_write,
            }),
        });
    }
    let (family, entry) = metadata(provider, model_id)?;
    entry
        .pricing
        .map(|(input, output, cache_read, cache_write)| ModelPricing {
            input,
            output,
            cache_read,
            cache_write,
            long_context: long_context(family, entry.prefix),
        })
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

/// Whether a catalog entry or the reviewed price table names `model_id`
/// itself, or a dated snapshot of it, for any vendor (a router's
/// `vendor/model` included). Unlike `lookup`, an id that merely starts with a
/// listed family (`gpt-5.4-acme-tuned`, `ft:gpt-5.4:acme`) does not count.
#[must_use]
pub fn ships(model_id: &str) -> bool {
    // A router's `vendor/model` only for the vendors listed here: any other
    // prefix or a deeper path can name an account or a deployment.
    let Some(canonical) = (match model_id.split('/').collect::<Vec<_>>()[..] {
        [model] | ["openai" | "anthropic" | "google" | "gemini", model] => Some(model),
        _ => None,
    }) else {
        return false;
    };
    reviewed(model_id).is_some()
        || CATALOG.iter().any(|(provider, entries)| {
            let id = if *provider == "anthropic" {
                canonical.replace('.', "-")
            } else {
                canonical.to_owned()
            };
            entries.iter().any(|entry| {
                id == entry.prefix || id.strip_prefix(entry.prefix).is_some_and(is_snapshot)
            })
        })
}

/// The provider's own entry, else the first native catalog that knows the id
/// without its vendor prefix (`openai/gpt-6.1-sol` through a router).
fn metadata<'a>(provider: &'a str, model_id: &str) -> Option<(&'a str, &'static CatalogEntry)> {
    let canonical = model_id.split_once('/').map_or(model_id, |(_, id)| id);
    lookup(provider, model_id)
        .map(|entry| (provider, entry))
        .or_else(|| {
            CATALOG.iter().find_map(|(provider, _)| {
                lookup(provider, canonical).map(|entry| (*provider, entry))
            })
        })
}

/// Fill catalog metadata into a `ModelInfo`, leaving explicitly set values
/// untouched.
#[must_use]
pub fn annotate(provider: &str, mut info: ModelInfo) -> ModelInfo {
    info.display_name = haarness_protocol::model_display_name(&info.id, Some(&info.display_name));
    if let Some((_, entry)) = metadata(provider, &info.id) {
        info.context_window.get_or_insert(entry.context_window);
        info.max_output_tokens
            .get_or_insert(entry.max_output_tokens);
    }
    if info.pricing.is_none() {
        info.pricing = pricing(provider, &info.id);
    }
    info
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::{annotate, lookup, pricing, pricing_on, reviewed, ships};

    fn day(date: &str) -> NaiveDate {
        date.parse().unwrap()
    }

    #[test]
    fn only_listed_models_and_their_snapshots_are_shipped() {
        assert!(ships("gpt-5.4-mini"));
        assert!(ships("gpt-5.4-mini-2026-01-01"));
        assert!(ships("claude-sonnet-4-6-20260201"));
        assert!(ships("anthropic/claude-sonnet-4.5"));
        assert!(ships("claude-haiku-4-5"));
        assert!(!ships("gpt-5.4-acme-tuned"));
        assert!(!ships("ft:gpt-4o-mini:acme-corp:project-falcon:AbC123"));
        assert!(!ships("accounts/acme/models/claude-opus-5-5"));
        assert!(!ships("acme-secret-org/claude-opus-5-5"));
        assert!(!ships(
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/acmeprod"
        ));
        assert!(!ships("acme-internal-deployment"));
    }

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
            ("gemini", "gemini-4-argon", 2.0, 10.0),
            ("gemini", "gemini-3.8-flash", 0.75, 3.75),
            ("gemini", "gemini-3.1-pro-preview", 2.0, 12.0),
            // Fallback prices for models the table does not list.
            ("openai", "gpt-5.4", 2.5, 15.0),
            ("anthropic", "claude-opus-4-1-20250805", 15.0, 75.0),
        ] {
            let price = pricing_on(provider, model, day("2026-10-15"))
                .unwrap_or_else(|| panic!("{provider}/{model} has no price"));
            assert!(
                (price.input - input).abs() < f64::EPSILON
                    && (price.output - output).abs() < f64::EPSILON,
                "{provider}/{model}: {price:?}"
            );
        }
    }

    #[test]
    fn scheduled_price_changes_apply_from_their_day() {
        let argon = |date: &str| pricing_on("gemini", "gemini-4-argon", day(date)).unwrap();
        let november = argon("2026-11-30");
        assert!((november.input - 2.0).abs() < f64::EPSILON);
        assert!((november.output - 10.0).abs() < f64::EPSILON);
        assert!((november.cache_read - 0.1).abs() < f64::EPSILON);
        assert!((november.cache_write - 2.0).abs() < f64::EPSILON);
        for date in ["2026-12-01", "2027-06-30"] {
            let december = argon(date);
            assert!((december.input - 4.0).abs() < f64::EPSILON, "{date}");
            assert!((december.output - 20.0).abs() < f64::EPSILON, "{date}");
            assert!((december.cache_read - 0.2).abs() < f64::EPSILON, "{date}");
            assert!((december.cache_write - 4.0).abs() < f64::EPSILON, "{date}");
            assert!(december.long_context.is_none(), "{date}");
        }
        // Routed ids and aliases follow the same schedule.
        let routed = pricing_on("router", "google/gemini-4-argon", day("2026-12-01")).unwrap();
        assert!((routed.input - 4.0).abs() < f64::EPSILON);
        let flash = |date: &str| {
            pricing_on("gemini", "gemini-3.8-flash", day(date))
                .unwrap()
                .input
        };
        assert!((flash("2026-12-31") - 0.75).abs() < f64::EPSILON);
        assert!((flash("2027-01-01") - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_change_replaces_every_rate_and_the_latest_one_wins() {
        let price: super::ReviewedPrice = serde_json::from_str(
            r#"{
                "id": "openai/example",
                "input": "1", "output": "2", "cache_read": "0.1", "cache_write": "1",
                "long_context": {"above_input_tokens": 1000, "input": "2", "output": "4", "cache_read": "0.2", "cache_write": "2"},
                "changes": [
                    {"from": "2026-12-01", "input": "3", "output": "6", "cache_read": "0.3", "cache_write": "3",
                     "long_context": {"above_input_tokens": 2000, "input": "6", "output": "12", "cache_read": "0.6", "cache_write": "6"}},
                    {"from": "2027-01-01", "input": "5", "output": "10", "cache_read": "0.5", "cache_write": "5"}
                ],
                "source": "https://example.com/pricing",
                "checked": "2026-10-15"
            }"#,
        )
        .unwrap();
        let input = |date: &str| price.rates_on(day(date)).input;
        assert!((input("2026-11-30") - 1.0).abs() < f64::EPSILON);
        assert!((input("2026-12-01") - 3.0).abs() < f64::EPSILON);
        assert!((input("2026-12-31") - 3.0).abs() < f64::EPSILON);
        assert!((input("2027-01-01") - 5.0).abs() < f64::EPSILON);
        let tier = price.rates_on(day("2026-12-15")).long_context.as_ref();
        assert_eq!(tier.unwrap().above_input_tokens, 2000);
        // A change that lists no tier ends the tier.
        assert!(price.rates_on(day("2027-01-01")).long_context.is_none());
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
        // Cloud workspaces name models the OpenRouter way: dots, not dashes.
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

    #[test]
    fn reviewed_table_matches_ids_aliases_and_snapshots_only() {
        let input = |provider: &str, model: &str| pricing(provider, model).map(|price| price.input);
        // Aliases and dated snapshots of a listed model.
        assert_eq!(input("openai", "gpt-5.6"), Some(4.0));
        assert_eq!(input("gemini", "gemini-3.1-flash-lite-preview"), Some(0.25));
        assert_eq!(input("openai", "gpt-5.6-sol-2026-09-01"), Some(4.0));
        assert_eq!(input("anthropic", "claude-haiku-4-5-20251001"), Some(1.0));
        assert_eq!(input("bedrock", "anthropic/claude-sonnet-4.5"), Some(3.0));
        // A different model that merely starts like a listed one is not it.
        assert!(reviewed("gpt-6.1-sol-pro").is_none());
        assert!(reviewed("claude-opus-4-1-20250805").is_none());
        assert!(reviewed("gemini-3.1-pro-preview-customtools").is_none());
        // Cache writes cost the 5-minute write price, or input where nothing is added.
        let sol = pricing("openai", "gpt-6.1-sol").unwrap();
        assert!((sol.cache_write - 2.5).abs() < f64::EPSILON);
        assert!((sol.long_context.unwrap().cache_write - 5.0).abs() < f64::EPSILON);
        let flash = pricing("gemini", "gemini-3.8-flash").unwrap();
        assert!((flash.cache_write - flash.input).abs() < f64::EPSILON);
        for price in super::REVIEWED.iter() {
            // Haarness Cloud refuses a table whose changes are out of order.
            assert!(
                price
                    .changes
                    .windows(2)
                    .all(|pair| pair[0].from < pair[1].from),
                "{}: changes must be in date order, one per date",
                price.id
            );
            let schedule = std::iter::once((None, &price.rates)).chain(
                price
                    .changes
                    .iter()
                    .map(|change| (Some(change.from), &change.rates)),
            );
            for (from, rates) in schedule {
                let all = [
                    rates.input,
                    rates.output,
                    rates.cache_read,
                    rates.cache_write,
                ];
                assert!(
                    all.iter().all(|rate| rate.is_finite() && *rate >= 0.0),
                    "{} {from:?}",
                    price.id
                );
                assert!(rates.cache_read <= rates.input, "{} {from:?}", price.id);
                if let Some(tier) = &rates.long_context {
                    assert!(tier.input > rates.input, "{} {from:?}", price.id);
                }
            }
        }
    }

    #[test]
    fn catalog_prices_only_models_the_table_does_not_list() {
        for (provider, entries) in super::CATALOG {
            for entry in *entries {
                assert!(
                    entry.pricing.is_none() || reviewed(entry.prefix).is_none(),
                    "{provider}/{}: priced in model-prices.json, drop the catalog price",
                    entry.prefix
                );
            }
        }
        for (provider, prefix, ..) in super::LONG_CONTEXT {
            assert!(reviewed(prefix).is_none(), "{provider}/{prefix}");
        }
    }

    #[test]
    fn price_table_matches_haarness_cloud() {
        // Present when haarness-cloud is checked out beside this repository.
        let cloud = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../haarness-cloud/config/model-prices.json");
        if let Ok(cloud) = std::fs::read_to_string(cloud) {
            assert!(
                cloud == super::PRICE_TABLE,
                "model-prices.json differs from Haarness Cloud's; run scripts/check-model-prices.py --sync"
            );
        }
    }
}
