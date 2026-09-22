//! Friendly labels shared with Desktop. Provider names win; unknown IDs stay intact.
use std::{collections::HashMap, sync::LazyLock};
static NAMES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("model-names.json")).expect("valid embedded model names")
});
pub fn model_display_name(id: &str, supplied: Option<&str>) -> String {
    if let Some(name) = supplied.filter(|name| !name.trim().is_empty() && *name != id) {
        return name.to_owned();
    }
    NAMES.get(id).cloned().unwrap_or_else(|| id.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_names_preserve_live_names_and_unknown_ids() {
        assert_eq!(model_display_name("gpt-6-astra", None), "GPT-6 Astra");
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
