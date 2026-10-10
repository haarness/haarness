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
        .cloned()
        .unwrap_or_else(|| id.to_owned())
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
}
