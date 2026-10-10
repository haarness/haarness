//! `/connect`: pick a provider, pick how, paste a key or sign in, and land in
//! the model picker. The server owns the catalog and the OAuth handshake; this
//! module owns the steps, the polling, and what happens when it succeeds.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::ApiClient,
    state::{App, OptionEntry, OptionsKind, Overlay},
};

/// How often the wait screen asks the server whether the sign-in finished.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Sentinel values in the provider picker that are not providers.
const CUSTOM: &str = "\u{0}custom";
const ROUTER: &str = "\u{0}router";
const MANAGE: &str = "\u{0}manage";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectMethod {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectProvider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub models: usize,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub methods: Vec<ConnectMethod>,
    #[serde(default)]
    pub key_url: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Place on the picker's "Popular" shelf; `None` is one of the rest.
    #[serde(default)]
    pub popular: Option<u8>,
}

impl ConnectProvider {
    /// One line under the provider's name in the picker.
    fn status(&self) -> String {
        if self.connected {
            let how = match self.source.as_deref() {
                Some("oauth") => "signed in",
                Some("api" | "legacy api") => "API key",
                Some("environment") => "from the environment",
                _ => "",
            };
            let models = match self.models {
                0 => String::new(),
                1 => "1 model".into(),
                count => format!("{count} models"),
            };
            return ["Connected", how, &models]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" · ");
        }
        if let Some(oauth) = self.methods.iter().find(|method| method.kind == "oauth") {
            return if self.methods.iter().any(|method| method.kind == "api") {
                format!("{} · or use an API key", oauth.label)
            } else if oauth.description.is_empty() {
                oauth.label.clone()
            } else {
                // A sign-in-only provider is chosen for what it gives, and
                // the name beside it already says where the sign-in goes.
                // The first clause is the gist and fits an 80-column card.
                oauth
                    .description
                    .split(" · ")
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            };
        }
        if self.methods.is_empty() {
            return self.note.clone().unwrap_or_else(|| "Not available".into());
        }
        "API key".into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoginState {
    Pending,
    Connected,
    Failed(String),
}

/// A sign-in the server is running on our behalf.
#[derive(Clone, Debug)]
pub(crate) struct PendingLogin {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub method: String,
    pub url: String,
    pub code: String,
    pub state: LoginState,
    pub started: Instant,
    pub(crate) last_poll: Option<Instant>,
    /// Whether we managed to hand the URL to a browser ourselves.
    pub opened: bool,
}

#[derive(Clone, Default)]
pub(crate) struct ConnectFlow {
    pub catalog: Vec<ConnectProvider>,
    /// The provider chosen in the picker; the method step and key prompt
    /// belong to it.
    pub provider: Option<ConnectProvider>,
    pub login: Option<PendingLogin>,
    /// Opened because nothing was connected yet, so success should also fix
    /// the session's model.
    pub onboarding: bool,
    /// Why the key prompt did not take the last key, shown under the field
    /// until the key is edited.
    pub key_error: Option<String>,
}

/// Whether the workspace server is on this machine, so a browser callback to
/// `localhost` reaches it. A remote server needs the device-code method.
fn server_is_local(client: &ApiClient) -> bool {
    url::Url::parse(&client.server_url()).is_ok_and(|url| {
        matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        )
    })
}

/// Whether this terminal probably has no browser of its own.
pub(crate) fn over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

impl App {
    /// Open the provider picker. `preselect` skips straight to that provider's
    /// method step (`/connect openai`).
    pub(crate) async fn open_connect_picker(
        &mut self,
        preselect: Option<&str>,
        client: &ApiClient,
    ) -> Result<()> {
        self.connect.provider = None;
        self.connect.login = None;
        match client
            .manage("providers", &json!({"action":"connect"}))
            .await
            .ok()
            .and_then(|view| {
                serde_json::from_value::<Vec<ConnectProvider>>(view["data"]["providers"].clone())
                    .ok()
            }) {
            Some(catalog) if !catalog.is_empty() => self.connect.catalog = catalog,
            // An older server, or no server at all: the picker still opens
            // from what this client already knows, and methods are resolved
            // when a provider is chosen.
            _ => {
                self.connect.catalog = self
                    .providers
                    .iter()
                    .map(|provider| ConnectProvider {
                        id: provider.id.clone(),
                        name: provider.name.clone(),
                        connected: provider.authenticated,
                        models: provider.models.len(),
                        ..ConnectProvider::default()
                    })
                    .collect();
            }
        }
        if let Some(id) = preselect {
            let id = id.trim().to_ascii_lowercase();
            if let Some(provider) = self.connect.catalog.iter().find(|provider| {
                provider.id.eq_ignore_ascii_case(&id) || provider.name.eq_ignore_ascii_case(&id)
            }) {
                let provider = provider.clone();
                return self.choose_connect_provider(provider, client).await;
            }
            self.notice = format!("No provider called {id} · pick one from the list");
        }
        // Connected rows are marked in the picker's gutter, so every name
        // starts in the same column.
        let mut entries: Vec<OptionEntry> = self
            .connect
            .catalog
            .iter()
            .map(|provider| OptionEntry {
                label: provider.name.clone(),
                description: provider.status(),
                value: provider.id.clone(),
            })
            .collect();
        entries.push(OptionEntry {
            label: "Custom endpoint…".into(),
            description: "Any OpenAI-, Anthropic- or Gemini-compatible API".into(),
            value: CUSTOM.into(),
        });
        entries.push(OptionEntry {
            label: "Haarness Router…".into(),
            description: "Self-hosted · one address for every provider".into(),
            value: ROUTER.into(),
        });
        entries.push(OptionEntry {
            label: "Manage providers…".into(),
            description: "Disconnect, edit or remove providers".into(),
            value: MANAGE.into(),
        });
        self.open_overlay(Overlay::Options {
            title: if self.any_provider_connected() {
                "Connect a provider"
            } else {
                "Welcome to Haarness"
            }
            .into(),
            kind: OptionsKind::ConnectProvider,
            entries,
        });
        Ok(())
    }

    /// Whether a picker row is a provider that already works. The view marks
    /// these in the gutter.
    pub(crate) fn connect_row_connected(&self, value: &str) -> bool {
        self.connect
            .catalog
            .iter()
            .any(|provider| provider.id == value && provider.connected)
    }

    /// The shelf a picker row belongs to: popular providers, every other
    /// provider, then the rows that are not a provider at all. `None` when
    /// the catalog carries no shelf, as an older server's does not.
    pub(crate) fn connect_row_group(&self, value: &str) -> Option<&'static str> {
        if !self
            .connect
            .catalog
            .iter()
            .any(|provider| provider.popular.is_some())
        {
            return None;
        }
        Some(
            match self
                .connect
                .catalog
                .iter()
                .find(|provider| provider.id == value)
            {
                Some(provider) if provider.popular.is_some() => "Popular",
                Some(_) => "More providers",
                None => "Something else",
            },
        )
    }

    /// A row of the provider picker was chosen.
    pub(crate) async fn execute_connect_choice(
        &mut self,
        value: &str,
        client: &ApiClient,
    ) -> Result<()> {
        match value {
            CUSTOM => {
                self.open_management("providers", &json!({"action":"form"}), client)
                    .await
            }
            ROUTER => {
                self.open_management("providers", &json!({"action":"router_form"}), client)
                    .await
            }
            MANAGE => {
                self.open_management("providers", &json!({"action":"view"}), client)
                    .await
            }
            id => {
                let Some(provider) = self
                    .connect
                    .catalog
                    .iter()
                    .find(|provider| provider.id == id)
                    .cloned()
                else {
                    self.notice = "That provider is no longer listed".into();
                    return Ok(());
                };
                self.choose_connect_provider(provider, client).await
            }
        }
    }

    async fn choose_connect_provider(
        &mut self,
        mut provider: ConnectProvider,
        client: &ApiClient,
    ) -> Result<()> {
        if provider.methods.is_empty() && provider.note.is_none() {
            // The picker was built offline; ask the server for this one
            // provider's story now, and fall back to a plain key prompt.
            if let Some(fresh) = client
                .manage("providers", &json!({"action":"connect","name":provider.id}))
                .await
                .ok()
                .and_then(|view| {
                    serde_json::from_value::<Vec<ConnectProvider>>(
                        view["data"]["providers"].clone(),
                    )
                    .ok()
                })
                .and_then(|mut rows| rows.pop())
            {
                provider = fresh;
            } else {
                provider.methods = vec![ConnectMethod {
                    id: "api".into(),
                    kind: "api".into(),
                    label: "Use an API key".into(),
                    description: "Paste a key from the provider console".into(),
                }];
            }
        }
        let local = server_is_local(client);
        // A browser callback only works when the browser and the server share
        // a machine. Elsewhere the device code is the sign-in.
        let methods: Vec<ConnectMethod> = provider
            .methods
            .iter()
            .filter(|method| local || method.id != "browser")
            .cloned()
            .collect();
        self.connect.provider = Some(provider.clone());
        match methods.len() {
            0 => {
                self.notice = provider
                    .note
                    .clone()
                    .unwrap_or_else(|| format!("{} cannot be connected from here", provider.name));
                Ok(())
            }
            1 => {
                let method = methods[0].clone();
                self.choose_connect_method(&method.id, client).await
            }
            _ => {
                let entries = methods
                    .iter()
                    .map(|method| OptionEntry {
                        label: method.label.clone(),
                        description: method.description.clone(),
                        value: method.id.clone(),
                    })
                    .collect::<Vec<_>>();
                self.open_overlay(Overlay::Options {
                    title: format!("Connect {}", provider.name),
                    kind: OptionsKind::ConnectMethod,
                    entries,
                });
                // Over SSH the browser lives elsewhere: start on the code.
                if over_ssh() {
                    if let Some(index) = methods.iter().position(|method| method.id == "device") {
                        self.selection = index;
                    }
                }
                Ok(())
            }
        }
    }

    /// A sign-in method was chosen for the current provider.
    pub(crate) async fn choose_connect_method(
        &mut self,
        method: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let Some(provider) = self.connect.provider.clone() else {
            return Ok(());
        };
        if method == "api" {
            let label = provider
                .methods
                .iter()
                .find(|candidate| candidate.id == "api")
                .map_or_else(
                    || "API key".to_owned(),
                    |candidate| {
                        ["Use an ", "Use a ", "Use "]
                            .iter()
                            .find_map(|prefix| candidate.label.strip_prefix(prefix))
                            .unwrap_or(&candidate.label)
                            .to_owned()
                    },
                );
            self.open_overlay(Overlay::Options {
                title: format!("Connect {}", provider.name),
                kind: OptionsKind::ConnectApiKey,
                entries: vec![OptionEntry {
                    label: "Connect".into(),
                    description: if server_is_local(client) {
                        format!("Save the {label} on this computer")
                    } else {
                        format!("Save the {label} on the server")
                    },
                    value: "submit".into(),
                }],
            });
            self.connect.key_error = None;
            return Ok(());
        }
        let view = match client
            .manage(
                "providers",
                &json!({"action":"login","name":provider.id,"method":method}),
            )
            .await
        {
            Ok(view) => view,
            Err(error) => {
                // The sign-in screen already knows how to say a sign-in
                // failed and offer another try; it also keeps the server's
                // reason, which is the part that says what to do.
                self.connect.login = Some(PendingLogin {
                    id: String::new(),
                    provider: provider.id.clone(),
                    name: provider.name.clone(),
                    method: method.to_owned(),
                    url: String::new(),
                    code: String::new(),
                    state: LoginState::Failed(format!("{error:#}")),
                    started: Instant::now(),
                    last_poll: None,
                    opened: false,
                });
                self.open_overlay(Overlay::ProviderLogin);
                return Ok(());
            }
        };
        let data = &view["data"];
        let url = data["url"].as_str().unwrap_or_default().to_owned();
        let mut login = PendingLogin {
            id: data["id"].as_str().unwrap_or_default().to_owned(),
            provider: provider.id.clone(),
            name: provider.name.clone(),
            method: data["method"].as_str().unwrap_or(method).to_owned(),
            url: url.clone(),
            code: data["code"].as_str().unwrap_or_default().to_owned(),
            state: LoginState::Pending,
            started: Instant::now(),
            last_poll: None,
            opened: false,
        };
        anyhow::ensure!(
            !login.id.is_empty() && !url.is_empty(),
            "The server did not start a sign-in"
        );
        if !over_ssh() {
            login.opened = crate::terminal::open_browser(&url);
        }
        self.connect.login = Some(login);
        self.open_overlay(Overlay::ProviderLogin);
        Ok(())
    }

    /// The typed API key was submitted.
    pub(crate) async fn submit_connect_key(&mut self, key: &str, client: &ApiClient) -> Result<()> {
        let Some(provider) = self.connect.provider.clone() else {
            return Ok(());
        };
        let key = key.trim();
        if key.is_empty() {
            self.choose_connect_method("api", client).await?;
            self.connect.key_error = Some("Paste a key first.".into());
            return Ok(());
        }
        if let Err(error) = client
            .manage(
                "providers",
                &json!({"action":"credential","name":provider.id,"key":key}),
            )
            .await
        {
            // Keep the prompt and the key: the reason sits under the field,
            // where the fix is made.
            self.choose_connect_method("api", client).await?;
            key.clone_into(&mut self.overlay_filter);
            self.connect.key_error = Some(
                if error.downcast_ref::<crate::api::ApiRefusal>().is_some() {
                    error.to_string()
                } else {
                    format!("Could not save the key: {error:#}")
                },
            );
            return Ok(());
        }
        self.finish_connect(&provider.id, client).await
    }

    /// Called from the frame tick while the wait screen is open.
    pub(crate) async fn poll_login(&mut self, client: &ApiClient) -> Result<()> {
        let Some(login) = self.connect.login.as_mut() else {
            return Ok(());
        };
        if login.state != LoginState::Pending
            || login
                .last_poll
                .is_some_and(|at| at.elapsed() < POLL_INTERVAL)
        {
            return Ok(());
        }
        login.last_poll = Some(Instant::now());
        let id = login.id.clone();
        let view = match client
            .manage("providers", &json!({"action":"login_status","id":id}))
            .await
        {
            Ok(view) => view,
            Err(error) => {
                if let Some(login) = self.connect.login.as_mut() {
                    login.state = LoginState::Failed(format!("{error:#}"));
                }
                return Ok(());
            }
        };
        let state = view["data"]["state"].as_str().unwrap_or("pending");
        let error = view["data"]["error"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| "sign-in failed".into());
        let provider = self
            .connect
            .login
            .as_ref()
            .map(|login| login.provider.clone())
            .unwrap_or_default();
        match state {
            "connected" => {
                if let Some(login) = self.connect.login.as_mut() {
                    login.state = LoginState::Connected;
                }
                self.finish_connect(&provider, client).await?;
            }
            "failed" | "cancelled" => {
                if let Some(login) = self.connect.login.as_mut() {
                    login.state = LoginState::Failed(error);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Leave the wait screen and tell the server to stop waiting.
    pub(crate) async fn cancel_login(&mut self, client: &ApiClient) {
        if let Some(login) = self.connect.login.take() {
            if login.state == LoginState::Pending {
                let _ = client
                    .manage("providers", &json!({"action":"login_cancel","id":login.id}))
                    .await;
                self.notice = format!("Sign-in to {} cancelled", login.name);
            }
        }
        self.close_overlay();
    }

    /// Retry the same method after a failed sign-in.
    pub(crate) async fn retry_login(&mut self, client: &ApiClient) -> Result<()> {
        let Some(login) = self.connect.login.take() else {
            return Ok(());
        };
        if self.connect.provider.is_none() {
            self.connect.provider = self
                .connect
                .catalog
                .iter()
                .find(|provider| provider.id == login.provider)
                .cloned();
        }
        self.choose_connect_method(&login.method, client).await
    }

    /// Open the sign-in page again, or copy the code, from the wait screen.
    pub(crate) fn reopen_login_browser(&mut self) {
        if let Some(login) = self.connect.login.as_mut() {
            login.opened = crate::terminal::open_browser(&login.url);
            self.notice = if login.opened {
                "Opened the sign-in page in your browser".into()
            } else {
                "Could not open a browser here · open the link by hand".into()
            };
        }
    }

    pub(crate) fn copy_login_code(&mut self) {
        if let Some(login) = &self.connect.login {
            let text = if login.code.is_empty() {
                login.url.clone()
            } else {
                login.code.clone()
            };
            crate::terminal::copy_to_clipboard(&text);
            self.notice = if login.code.is_empty() {
                "Link copied".into()
            } else {
                "Code copied".into()
            };
        }
    }

    /// A credential is in place: refresh the catalog, celebrate, and move on
    /// to choosing a model.
    pub(crate) async fn finish_connect(
        &mut self,
        provider: &str,
        client: &ApiClient,
    ) -> Result<()> {
        if let Ok(providers) = client.refresh_providers().await {
            self.providers = providers;
        } else if let Ok(providers) = client.providers().await {
            self.providers = providers;
        }
        let name = self
            .providers
            .iter()
            .find(|candidate| candidate.id == provider)
            .map_or_else(|| provider.to_owned(), |candidate| candidate.name.clone());
        if provider == "haarness" && self.resume_remote_control(client) {
            self.connect.login = None;
            self.connect.provider = None;
            self.notice = format!("{name} connected · sharing this chat");
            return Ok(());
        }
        let models: Vec<String> = self
            .providers
            .iter()
            .filter(|candidate| candidate.id == provider && candidate.authenticated)
            .flat_map(|candidate| candidate.models.iter().map(|model| model.id.clone()))
            .collect();
        self.connect.login = None;
        self.connect.provider = None;
        let session_connected = self
            .providers
            .iter()
            .any(|candidate| candidate.id == self.thread.provider && candidate.authenticated);
        if models.is_empty() {
            self.notice = format!("{name} connected · no models listed yet");
            self.close_overlay();
            return Ok(());
        }
        // A session without a working model gets the provider's first one so
        // the next prompt just works; the picker still opens for a better one.
        if !session_connected || self.connect.onboarding {
            let first = format!("{provider}/{}", models[0]);
            self.set_model(&first, client).await?;
        }
        self.connect.onboarding = false;
        self.open_overlay(Overlay::Models);
        self.overlay_filter = format!("{provider}/");
        self.notice = format!("{name} connected · pick a model");
        Ok(())
    }

    /// Whether any provider can serve a prompt right now.
    pub(crate) fn any_provider_connected(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.authenticated && !provider.models.is_empty())
    }

    /// Hint rows above the API-key prompt: where the key comes from and the
    /// environment variables that work instead.
    pub(crate) fn connect_key_hints(&self) -> Vec<String> {
        let Some(provider) = &self.connect.provider else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        // Coming back to a provider that already has a key must not look
        // like a first connection: a new key replaces the saved one.
        if provider.connected && matches!(provider.source.as_deref(), Some("api" | "legacy api")) {
            lines.push(
                self.language
                    .text("A key is already saved · paste a new one to replace it")
                    .to_owned(),
            );
        }
        if let Some(url) = &provider.key_url {
            lines.push(format!("Create a key  {url}"));
        }
        if !provider.env.is_empty() {
            lines.push(format!("Or set  {}", provider.env.join(" / ")));
        }
        if let Some(note) = &provider.note {
            lines.push(note.clone());
        }
        lines
    }
}

/// Parse a `/connect` argument into the provider to preselect.
pub(crate) fn connect_target(arguments: &str) -> Option<&str> {
    arguments
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn picker_status_reads_like_a_sentence() {
        let mut provider = ConnectProvider {
            id: "openai".into(),
            name: "OpenAI".into(),
            connected: true,
            models: 12,
            source: Some("oauth".into()),
            ..ConnectProvider::default()
        };
        assert_eq!(provider.status(), "Connected · signed in · 12 models");
        provider.connected = false;
        provider.source = None;
        provider.methods = vec![
            ConnectMethod {
                id: "browser".into(),
                kind: "oauth".into(),
                label: "Sign in with ChatGPT".into(),
                description: String::new(),
            },
            ConnectMethod {
                id: "api".into(),
                kind: "api".into(),
                label: "Use an API key".into(),
                description: String::new(),
            },
        ];
        assert_eq!(
            provider.status(),
            "Sign in with ChatGPT · or use an API key"
        );
        // Haarness Cloud is an account sign-in only; offering a key would
        // send people looking for one that does not exist. Its row says what
        // signing in gives, which its name beside it does not.
        provider.methods.truncate(1);
        provider.methods[0].label = "Sign in to Haarness".into();
        assert_eq!(provider.status(), "Sign in to Haarness");
        provider.methods[0].description =
            "Cloud models on your Haarness plan · no API key needed".into();
        assert_eq!(provider.status(), "Cloud models on your Haarness plan");
        provider.methods.clear();
        provider.note = Some("Local runtime".into());
        assert_eq!(provider.status(), "Local runtime");
    }

    #[test]
    fn connect_target_takes_the_first_word() {
        assert_eq!(connect_target("  openai extra"), Some("openai"));
        assert_eq!(connect_target(""), None);
    }

    #[test]
    fn catalog_rows_deserialize_from_the_management_view() {
        let value: Value = json!({"id":"xai","name":"xAI","connected":false,"models":3,
            "methods":[{"id":"browser","kind":"oauth","label":"Sign in with xAI","description":"SuperGrok"}],
            "keyUrl":"https://console.x.ai","env":["XAI_API_KEY"],"note":null,"popular":4});
        let provider: ConnectProvider = serde_json::from_value(value).unwrap();
        assert_eq!(provider.key_url.as_deref(), Some("https://console.x.ai"));
        assert_eq!(provider.env, ["XAI_API_KEY"]);
        assert_eq!(provider.methods[0].label, "Sign in with xAI");
    }
}
