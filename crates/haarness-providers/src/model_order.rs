//! Stable catalog order for dynamically discovered IDs, including routed IDs.
//! Group providers/families, compare generations numerically, then variants.
use std::cmp::{Ordering, Reverse};

pub(crate) fn compare(left: &str, right: &str) -> Ordering {
    key(left).cmp(&key(right))
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct ModelOrder<'a> {
    snapshot: bool,
    source: (u8, &'a str),
    family: (u8, &'a str),
    generation: Reverse<(u32, u32)>,
    variant: u8,
    preview: bool,
    id: &'a str,
}

fn key(id: &str) -> ModelOrder<'_> {
    let (source, model) = id.rsplit_once('/').unwrap_or(("", id));
    let source = match source {
        "" | "openai" => (0, source),
        "anthropic" => (1, source),
        "google" | "gemini" => (2, source),
        _ => (3, source),
    };
    let family = if model.starts_with("gpt-") || model.starts_with("chatgpt-") {
        (0, "gpt")
    } else if model.starts_with('o') && model.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
        (1, "o")
    } else if model.starts_with("claude-") {
        (0, "claude")
    } else if model.starts_with("gemini-") {
        (0, "gemini")
    } else if model.starts_with("gemma-") {
        (1, "gemma")
    } else {
        (
            2,
            model
                .split_once(|c: char| c.is_ascii_digit())
                .map_or(model, |(family, _)| family),
        )
    };
    let mut numbers = model
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty());
    // Four/eight-digit date components are snapshots, not model generations.
    let major = numbers
        .next()
        .filter(|part| part.len() < 4)
        .and_then(|part| part.parse().ok())
        .unwrap_or(0);
    let minor = numbers
        .next()
        .filter(|part| part.len() < 4)
        .and_then(|part| part.parse().ok())
        .unwrap_or(0);
    let variant = match family.1 {
        "gpt" if model.contains("-astra") => 0,
        "gpt" if model.contains("-sol") => 1,
        "gpt" if model.contains("-terra") => 2,
        "gpt" if model.contains("-luna") => 3,
        "claude" if model.contains("-opus") => 0,
        "claude" if model.contains("-sonnet") => 1,
        "claude" if model.contains("-fable") => 2,
        "claude" if model.contains("-haiku") => 3,
        _ if model.contains("-pro") => 0,
        _ if model.contains("-mini") || model.contains("-flash") && !model.contains("-lite") => 2,
        _ if model.contains("-nano") || model.contains("-lite") => 3,
        _ => 1,
    };
    ModelOrder {
        snapshot: super::is_snapshot(model),
        source,
        family,
        generation: Reverse((major, minor)),
        variant,
        preview: model.contains("preview"),
        id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routed_catalog_groups_generations_and_variants_instead_of_reverse_alphabetical_ids() {
        let expected = [
            "openai/gpt-6-astra",
            "openai/gpt-6-sol",
            "openai/gpt-6-luna",
            "openai/gpt-5.10",
            "openai/gpt-5.9",
            "openai/gpt-5.4-pro",
            "openai/gpt-5.4",
            "openai/gpt-5.4-mini",
            "openai/gpt-5.4-nano",
            "openai/o4-mini",
            "openai/o3-pro",
            "openai/o3",
            "anthropic/claude-opus-5.5",
            "anthropic/claude-sonnet-5",
            "anthropic/claude-opus-4.6",
            "anthropic/claude-sonnet-4.6",
            "google/gemini-4-argon",
            "google/gemini-3.8-flash",
            "google/gemini-3.5-flash",
            "google/gemini-3.5-flash-lite",
            "google/gemma-4-31b-it",
        ];
        let mut ids = expected.into_iter().rev().collect::<Vec<_>>();
        ids.sort_by(|left, right| compare(left, right));
        assert_eq!(ids, expected);
    }
}
