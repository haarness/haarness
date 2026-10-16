//! Friendly labels shared with Desktop. Provider names win; unknown IDs stay intact.
use std::{collections::HashMap, sync::LazyLock};
static NAMES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("model-names.json")).expect("valid embedded model names")
});

/// Public Cloud prefix, preserving the persisted provider identity for existing conversations.
pub fn provider_display_id(id: &str) -> &str {
    if id == "haarness" {
        "haarness-cloud"
    } else {
        id
    }
}

pub fn provider_storage_id(id: &str) -> &str {
    if id == "haarness-cloud" {
        "haarness"
    } else {
        id
    }
}

pub fn model_display_id(qualified: &str) -> String {
    qualified.split_once('/').map_or_else(
        || qualified.to_owned(),
        |(provider, model)| format!("{}/{model}", provider_display_id(provider)),
    )
}
pub fn model_display_name(id: &str, supplied: Option<&str>) -> String {
    if let Some(name) = supplied.filter(|name| !name.trim().is_empty() && *name != id) {
        return name.to_owned();
    }
    // Routers name a model `provider/model`; the bare id may be known.
    NAMES
        .get(id)
        .or_else(|| id.rsplit_once('/').and_then(|(_, model)| NAMES.get(model)))
        .or_else(|| claude_base_id(id).and_then(|model| NAMES.get(model)))
        .cloned()
        .unwrap_or_else(|| id.to_owned())
}

/// Bedrock writes a Claude id with a region and vendor prefix and a version
/// tag (`apac.anthropic.claude-sonnet-5-5-v1:0`, `anthropic.claude-opus-4-6-v1`)
/// in more combinations than the table lists; this is the bare id it names.
/// `None` when `id` is not such a Claude id.
fn claude_base_id(id: &str) -> Option<&str> {
    let model = id.rsplit_once('/').map_or(id, |(_, model)| model);
    let start = model.find("claude-")?;
    let vendor = match &model[..start] {
        "" | "anthropic." => true,
        prefix => prefix.strip_suffix(".anthropic.").is_some_and(|region| {
            !region.is_empty()
                && region
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        }),
    };
    if !vendor {
        return None;
    }
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let mut base = &model[start..];
    if let Some((head, revision)) = base.rsplit_once(':')
        && digits(revision)
    {
        base = head;
    }
    if let Some((head, version)) = base.rsplit_once("-v")
        && digits(version)
    {
        base = head;
    }
    (base != id).then_some(base)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_names_preserve_live_names_and_unknown_ids() {
        assert_eq!(model_display_name("gpt-6-astra", None), "GPT-6 Astra");
        // Routed ids fall back to the bare model's name; unknown ones stay intact.
        assert_eq!(model_display_name("openai/o3-pro", None), "o3 Pro");
        assert_eq!(
            model_display_name("router/openai/gpt-6.1-sol", None),
            "GPT-6.1 Sol"
        );
        assert_eq!(
            model_display_name("my-company/future-7", None),
            "my-company/future-7"
        );
        assert_eq!(
            model_display_name("claude-fable-5-1", Some("claude-fable-5-1")),
            "Claude Fable 5.1"
        );
        assert_eq!(
            model_display_name("gpt-6-astra", Some("Team deployment")),
            "Team deployment"
        );
        assert_eq!(
            model_display_name("private/model-unknown", None),
            "private/model-unknown"
        );
        assert_eq!(
            model_display_name("us.anthropic.claude-sonnet-4-5-20250929-v1:0", None),
            "Claude Sonnet 4.5 (2025-09-29)"
        );
    }

    #[test]
    fn bedrock_claude_ids_read_by_the_model_they_name() {
        for (id, name) in [
            ("apac.anthropic.claude-sonnet-5-5-v1:0", "Claude Sonnet 5.5"),
            ("us.anthropic.claude-sonnet-5-5-v1:0", "Claude Sonnet 5.5"),
            ("anthropic.claude-opus-4-6-v1", "Claude Opus 4.6"),
            ("jp.anthropic.claude-opus-4-6-v1", "Claude Opus 4.6"),
            (
                "us-gov.anthropic.claude-haiku-4-5-20251001-v1:0",
                "Claude Haiku 4.5 (2025-10-01)",
            ),
            (
                "amazon-bedrock/au.anthropic.claude-sonnet-4-5-20250929-v1:0",
                "Claude Sonnet 4.5 (2025-09-29)",
            ),
        ] {
            assert_eq!(model_display_name(id, Some(id)), name, "{id}");
        }
        // A Claude id the table does not know, and lookalikes, stay as written.
        for id in [
            "apac.anthropic.claude-sonnet-9-9-v1:0",
            "mycorp.claude-sonnet-5-5",
            "meta.llama3-70b-instruct-v1:0",
        ] {
            assert_eq!(model_display_name(id, None), id);
        }
    }
}
