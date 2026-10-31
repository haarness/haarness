//! What a client needs in order to connect a provider: which sign-in methods
//! exist, where an API key comes from, and which environment variables work
//! instead. Shared by the terminal, Desktop and the management API so every
//! surface tells the same story.

use serde::Serialize;

use crate::compatible_preset;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MethodKind {
    /// A browser or device-code sign-in that stores refreshable tokens.
    Oauth,
    /// A pasted API key or token.
    Api,
}

/// One way of connecting a provider. `id` is what a client sends back as
/// `method` when it starts a login; the rest is copy for the picker.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectMethod {
    pub id: &'static str,
    pub kind: MethodKind,
    pub label: &'static str,
    pub description: &'static str,
}

const fn method(
    id: &'static str,
    kind: MethodKind,
    label: &'static str,
    description: &'static str,
) -> ConnectMethod {
    ConnectMethod {
        id,
        kind,
        label,
        description,
    }
}

const API_KEY: ConnectMethod = method(
    "api",
    MethodKind::Api,
    "Use an API key",
    "Paste a key from the provider console",
);

/// Everything the connect picker shows for one provider.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectGuide {
    pub methods: Vec<ConnectMethod>,
    /// Where to create an API key, when the provider has a console for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_url: Option<&'static str>,
    /// Environment variables that connect the provider without storing a key.
    pub env: Vec<&'static str>,
    /// Extra guidance, for providers whose endpoint needs more than a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    /// Ranking on the picker's "popular" shelf; `None` sorts alphabetically
    /// after the shelf.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub popular: Option<u8>,
}

/// The connect story for a provider id. Unknown ids get the plain API-key
/// story, which is right for user-defined compatible endpoints.
#[must_use]
pub fn guide(provider: &str) -> ConnectGuide {
    match provider {
        "haarness" => ConnectGuide {
            methods: vec![
                method(
                    "browser",
                    MethodKind::Oauth,
                    "Sign in to Haarness",
                    "Cloud models on your Haarness plan · no API key needed",
                ),
                method(
                    "device",
                    MethodKind::Oauth,
                    "Sign in with a code",
                    "For SSH sessions and remote servers",
                ),
            ],
            popular: Some(0),
            ..ConnectGuide::default()
        },
        "openai" => ConnectGuide {
            methods: vec![
                method(
                    "browser",
                    MethodKind::Oauth,
                    "Sign in with ChatGPT",
                    "Use your ChatGPT Plus or Pro subscription",
                ),
                method(
                    "device",
                    MethodKind::Oauth,
                    "Sign in with a code",
                    "For SSH sessions and remote servers",
                ),
                method(
                    "api",
                    MethodKind::Api,
                    "Use an API key",
                    "Pay as you go through platform.openai.com",
                ),
            ],
            key_url: Some("https://platform.openai.com/api-keys"),
            env: vec!["OPENAI_API_KEY"],
            note: None,
            popular: Some(0),
        },
        "anthropic" => ConnectGuide {
            methods: vec![API_KEY],
            key_url: Some("https://console.anthropic.com/settings/keys"),
            env: vec!["ANTHROPIC_API_KEY"],
            note: None,
            popular: Some(1),
        },
        "gemini" => ConnectGuide {
            methods: vec![API_KEY],
            key_url: Some("https://aistudio.google.com/apikey"),
            env: vec![
                "GOOGLE_API_KEY",
                "GEMINI_API_KEY",
                "GOOGLE_GENERATIVE_AI_API_KEY",
            ],
            note: None,
            popular: Some(2),
        },
        "github-copilot" => ConnectGuide {
            methods: vec![
                method(
                    "browser",
                    MethodKind::Oauth,
                    "Sign in with GitHub",
                    "Use your Copilot subscription",
                ),
                method(
                    "device",
                    MethodKind::Oauth,
                    "Sign in with a code",
                    "For SSH sessions and remote servers",
                ),
                method(
                    "api",
                    MethodKind::Api,
                    "Use a GitHub token",
                    "A personal access token with Copilot access",
                ),
            ],
            key_url: Some("https://github.com/settings/tokens"),
            env: vec!["GITHUB_COPILOT_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
            note: None,
            popular: Some(3),
        },
        "xai" => ConnectGuide {
            methods: vec![
                method(
                    "browser",
                    MethodKind::Oauth,
                    "Sign in with xAI",
                    "Use your SuperGrok subscription",
                ),
                method(
                    "device",
                    MethodKind::Oauth,
                    "Sign in with a code",
                    "For SSH sessions and remote servers",
                ),
                method(
                    "api",
                    MethodKind::Api,
                    "Use an API key",
                    "Pay as you go through console.x.ai",
                ),
            ],
            key_url: Some("https://console.x.ai"),
            env: vec!["XAI_API_KEY"],
            note: None,
            popular: Some(4),
        },
        "azure" => ConnectGuide {
            methods: vec![API_KEY],
            key_url: Some("https://portal.azure.com"),
            env: vec!["AZURE_OPENAI_API_KEY", "AZURE_API_KEY"],
            note: Some(
                "Also set AZURE_RESOURCE_NAME or AZURE_OPENAI_BASE_URL so Haarness knows which deployment to call.",
            ),
            popular: None,
        },
        "google-vertex" => ConnectGuide {
            methods: vec![method(
                "api",
                MethodKind::Api,
                "Use an access token",
                "A Google Cloud OAuth access token",
            )],
            key_url: None,
            env: vec!["GOOGLE_OAUTH_ACCESS_TOKEN", "GOOGLE_ACCESS_TOKEN"],
            note: Some(
                "Also set GOOGLE_VERTEX_PROJECT (and optionally GOOGLE_VERTEX_LOCATION) for the project to bill.",
            ),
            popular: None,
        },
        "amazon-bedrock" => ConnectGuide {
            methods: vec![method(
                "api",
                MethodKind::Api,
                "Use an API key",
                "Paste a Bedrock API key from the AWS console",
            )],
            key_url: Some("https://console.aws.amazon.com/bedrock/home#/api-keys"),
            env: vec![
                "AWS_BEARER_TOKEN_BEDROCK",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
            ],
            note: Some("Standard AWS credentials and profiles are picked up automatically."),
            popular: None,
        },
        "ollama" | "lmstudio" => ConnectGuide {
            methods: Vec::new(),
            key_url: None,
            env: Vec::new(),
            note: Some("Local runtime · detected automatically while it is running"),
            popular: None,
        },
        "echo" => ConnectGuide {
            methods: Vec::new(),
            key_url: None,
            env: Vec::new(),
            note: Some("Built-in test provider"),
            popular: None,
        },
        other => {
            let preset = compatible_preset(other);
            ConnectGuide {
                methods: vec![API_KEY],
                key_url: preset.and_then(|preset| key_url(preset.id)),
                env: preset.map_or_else(Vec::new, |preset| preset.api_key_envs.to_vec()),
                note: None,
                popular: match other {
                    "openrouter" => Some(5),
                    "groq" => Some(6),
                    "deepseek" => Some(7),
                    "mistral" => Some(8),
                    _ => None,
                },
            }
        }
    }
}

/// The console page where a compatible preset's key is created.
fn key_url(preset: &str) -> Option<&'static str> {
    Some(match preset {
        "groq" => "https://console.groq.com/keys",
        "mistral" => "https://console.mistral.ai/api-keys",
        "deepseek" => "https://platform.deepseek.com/api_keys",
        "openrouter" => "https://openrouter.ai/settings/keys",
        "cerebras" => "https://cloud.cerebras.ai/platform",
        "sambanova" => "https://cloud.sambanova.ai/apis",
        "nvidia" => "https://build.nvidia.com/settings/api-keys",
        "zai" => "https://z.ai/manage-apikey/apikey-list",
        "deepinfra" => "https://deepinfra.com/dash/api_keys",
        "huggingface" => "https://huggingface.co/settings/tokens",
        "perplexity" => "https://www.perplexity.ai/settings/api",
        "moonshotai" => "https://platform.moonshot.ai/console/api-keys",
        "siliconflow" => "https://cloud.siliconflow.com/account/ak",
        "siliconflow-cn" => "https://cloud.siliconflow.cn/account/ak",
        "nebius" => "https://tokenfactory.nebius.com",
        "requesty" => "https://app.requesty.ai/api-keys",
        "ollama-cloud" => "https://ollama.com/settings/keys",
        "llama" => "https://llama.developer.meta.com/api-keys",
        "modelscope" => "https://modelscope.cn/my/myaccesstoken",
        "upstage" => "https://console.upstage.ai/api-keys",
        "digitalocean" => "https://cloud.digitalocean.com/account/api/tokens",
        "wandb" => "https://wandb.ai/authorize",
        "chutes" => "https://chutes.ai/app/api",
        _ => return None,
    })
}

/// Sort key for the connect picker: the popular shelf first in its fixed
/// order, then everything else by display name.
#[must_use]
pub fn rank(provider: &str, name: &str) -> (u8, String) {
    (
        guide(provider).popular.unwrap_or(u8::MAX),
        name.to_lowercase(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::COMPAT_PRESETS;

    #[test]
    fn subscription_providers_offer_browser_device_and_key() {
        for id in ["openai", "xai", "github-copilot"] {
            let guide = guide(id);
            let ids: Vec<_> = guide.methods.iter().map(|method| method.id).collect();
            assert_eq!(ids, ["browser", "device", "api"], "{id}");
        }
        for id in ["anthropic", "groq"] {
            assert!(
                guide(id)
                    .methods
                    .iter()
                    .all(|method| method.kind != MethodKind::Oauth),
                "{id}"
            );
        }
    }

    #[test]
    fn every_compatible_preset_has_a_key_story() {
        for preset in COMPAT_PRESETS {
            let guide = guide(preset.id);
            assert!(
                guide.methods.iter().any(|method| method.id == "api"),
                "{}",
                preset.id
            );
            assert_eq!(guide.env, preset.api_key_envs, "{}", preset.id);
            if preset.id != "xai" {
                assert_eq!(guide.methods.len(), 1, "{}", preset.id);
            }
        }
    }

    #[test]
    fn local_runtimes_and_unknown_ids_are_sensible() {
        assert!(guide("ollama").methods.is_empty());
        assert!(guide("ollama").note.is_some());
        let custom = guide("my-gateway");
        assert_eq!(custom.methods[0].id, "api");
        assert!(custom.key_url.is_none());
        assert!(custom.env.is_empty());
    }

    #[test]
    fn popular_shelf_precedes_alphabetical_rest() {
        let mut ids = [
            ("zai", "Z.AI"),
            ("anthropic", "Anthropic"),
            ("openai", "OpenAI"),
            ("cerebras", "Cerebras"),
        ];
        ids.sort_by_key(|(id, name)| rank(id, name));
        assert_eq!(
            ids.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ["openai", "anthropic", "cerebras", "zai"]
        );
    }
}
