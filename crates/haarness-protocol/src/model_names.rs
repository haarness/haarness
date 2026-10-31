//! Friendly labels shared with Desktop. Provider names win, then the table;
//! an id the table does not know reads by its brand when it names one
//! (`gpt-7-mini` is "GPT-7 Mini"), and any other stays intact.
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
        .or_else(|| {
            let base = claude_base_id(id)
                .unwrap_or_else(|| id.rsplit_once('/').map_or(id, |(_, model)| model));
            tidy_name(base)
        })
        .unwrap_or_else(|| id.to_owned())
}

/// Brands a model id starts with, as their names spell them. A brand marked
/// `true` joins its version with a hyphen (`GPT-7`, `GLM-6`).
const BRANDS: [(&str, &str, bool); 14] = [
    ("gpt", "GPT", true),
    ("glm", "GLM", true),
    ("claude", "Claude", false),
    ("gemini", "Gemini", false),
    ("gemma", "Gemma", false),
    ("grok", "Grok", false),
    ("kimi", "Kimi", false),
    ("mistral", "Mistral", false),
    ("devstral", "Devstral", false),
    ("codestral", "Codestral", false),
    ("sonar", "Sonar", false),
    ("deepseek", "DeepSeek", false),
    ("qwen", "Qwen", false),
    ("llama", "Llama", false),
];

/// A readable name for a model id the table does not know yet, so a model a
/// provider adds reads like its siblings without a release: `gpt-7-mini`
/// reads "GPT-7 Mini", `claude-opus-6-1` "Claude Opus 6.1",
/// `claude-sonnet-6-20270101` "Claude Sonnet 6 (2027-01-01)", `deepseek-v5`
/// "DeepSeek V5". `None` unless the id starts with a known brand (or is an
/// `o`-series id) and holds only lowercase letters, digits, `.` and `-`.
fn tidy_name(id: &str) -> Option<String> {
    let mut tokens = id.split(['-', '@']);
    let first = tokens.next()?;
    let (mut name, joined) =
        if let Some((_, brand, joined)) = BRANDS.iter().find(|(word, _, _)| *word == first) {
            ((*brand).to_owned(), *joined)
        } else if first.len() > 1
            && first.starts_with('o')
            && first[1..].bytes().all(|byte| byte.is_ascii_digit())
        {
            (first.to_owned(), false)
        } else {
            return None;
        };
    let mut words: Vec<String> = Vec::new();
    let mut date = None;
    // Claude writes a version as separate numbers (`4-5` is 4.5).
    let mut in_version = false;
    for token in tokens {
        if token.is_empty()
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.')
        {
            return None;
        }
        let digits = token.bytes().all(|byte| byte.is_ascii_digit());
        if digits && token.len() == 8 {
            date = Some(format!(
                "({}-{}-{})",
                &token[..4],
                &token[4..6],
                &token[6..]
            ));
            continue;
        }
        if token == "latest" {
            continue;
        }
        if digits && token.len() <= 2 {
            match words.last_mut() {
                Some(last) if in_version => {
                    last.push('.');
                    last.push_str(token);
                }
                _ => words.push(token.to_owned()),
            }
            in_version = true;
            continue;
        }
        in_version = false;
        words.push(tidy_word(token));
    }
    if joined
        && let Some(version) = words
            .first()
            .filter(|word| word.starts_with(|c: char| c.is_ascii_digit()) || *word == "OSS")
    {
        name = format!("{name}-{version}");
        words.remove(0);
    }
    let mut parts = vec![name];
    parts.extend(words);
    parts.extend(date);
    Some(parts.join(" "))
}

/// One word of a model id as a name writes it: `mini` is "Mini", `v5` "V5",
/// `120b` (a parameter count) "120B", `oss` "OSS"; a version stays as it is.
fn tidy_word(token: &str) -> String {
    if token == "oss" {
        return "OSS".into();
    }
    if let Some(count) = token.strip_suffix(['b', 'k', 'm'])
        && !count.is_empty()
        && count
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return token.to_ascii_uppercase();
    }
    let mut chars = token.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            first.to_ascii_uppercase().to_string() + chars.as_str()
        }
        _ => token.to_owned(),
    }
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
        // A Claude id the table does not know reads by its words; lookalikes
        // stay as written.
        assert_eq!(
            model_display_name("apac.anthropic.claude-sonnet-9-9-v1:0", None),
            "Claude Sonnet 9.9"
        );
        for id in ["mycorp.claude-sonnet-5-5", "meta.llama3-70b-instruct-v1:0"] {
            assert_eq!(model_display_name(id, None), id);
        }
    }

    #[test]
    fn a_model_new_to_the_table_reads_by_its_brand() {
        for (id, name) in [
            ("gpt-7-mini", "GPT-7 Mini"),
            ("gpt-6.2-sol", "GPT-6.2 Sol"),
            ("openai/gpt-7", "GPT-7"),
            ("o7-mini", "o7 Mini"),
            ("claude-opus-6", "Claude Opus 6"),
            ("claude-opus-6-1", "Claude Opus 6.1"),
            ("claude-sonnet-6-20270101", "Claude Sonnet 6 (2027-01-01)"),
            ("claude-sonnet-6@20270101", "Claude Sonnet 6 (2027-01-01)"),
            ("deepseek-v5", "DeepSeek V5"),
            ("gemini-5-pro-preview", "Gemini 5 Pro Preview"),
            ("grok-5-fast", "Grok 5 Fast"),
            ("glm-6", "GLM-6"),
            ("gpt-oss-240b", "GPT-OSS 240B"),
            ("kimi-k4", "Kimi K4"),
            ("qwen-4-coder", "Qwen 4 Coder"),
        ] {
            assert_eq!(model_display_name(id, None), name, "{id}");
            assert_eq!(model_display_name(id, Some(id)), name, "{id}");
        }
        // Ids that name no brand, or are not written as ids, stay intact.
        for id in [
            "future-7",
            "my-company/future-7",
            "GPT-7",
            "gpt-7 mini",
            "gpt-",
            "o-mini",
        ] {
            assert_eq!(model_display_name(id, None), id, "{id}");
        }
    }

    #[test]
    fn the_brand_reading_agrees_with_the_table() {
        // The table names a few ids its own way; every other bare id in it
        // reads the same by the generic rule, so a sibling it lacks reads alike.
        let own_way = [
            "gpt-5.6",
            "grok-4.20-0309-reasoning",
            "grok-4.20-0309-non-reasoning",
            "grok-4-1-fast-non-reasoning",
            "mistral-large-4-0",
        ];
        for (id, name) in NAMES.iter() {
            if own_way.contains(&id.as_str()) {
                continue;
            }
            if let Some(tidy) = tidy_name(id) {
                assert_eq!(&tidy, name, "{id}");
            }
        }
    }
}
