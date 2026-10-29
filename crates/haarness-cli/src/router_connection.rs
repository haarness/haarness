//! Router identity is independent of the wire protocol and connection credentials.
//! Cloud can add its own deployment/login here without changing model routing.
use anyhow::{Context, Result, bail, ensure};
use haarness_providers::auth::{Credential, CredentialStore};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex, time::Duration};

static WRITES: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Integration {
    HaarnessRouter { deployment: RouterDeployment },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RouterDeployment {
    SelfHosted,
}

pub(crate) struct ConnectRequest {
    pub url: String,
    pub name: String,
    pub key: String,
    pub project: bool,
}

/// Said when a router answers with an empty catalog: it runs, but holds no provider key yet. The connection is kept,
/// since its models appear on their own once the router has one.
pub(crate) const NO_MODELS_YET: &str = "The router has no provider keys yet. Set one where it runs, such as OPENAI_API_KEY or ANTHROPIC_API_KEY, and restart it: its models then appear here on their own.";

pub(crate) enum ConnectResult {
    NeedsKey,
    Connected {
        id: String,
        name: String,
        models: usize,
    },
}

pub(crate) fn normalize_url(value: &str) -> Result<String> {
    let value = value.trim();
    ensure!(!value.is_empty(), "Enter your router address");
    let value = if value.contains("://") {
        value.to_owned()
    } else {
        let local = value.starts_with("localhost")
            || value.starts_with("127.")
            || value.starts_with("[::1]");
        format!("{}://{value}", if local { "http" } else { "https" })
    };
    let mut url = reqwest::Url::parse(&value).context("Enter a valid router address")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use an HTTP or HTTPS router address without credentials, query parameters or fragments"
    );
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/v1") {
        path.to_owned()
    } else {
        format!("{path}/v1")
    };
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

async fn probe(url: &str, key: &str) -> Result<Option<usize>> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut request = client.get(format!("{url}/models"));
    if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    let mut response = request.send().await.map_err(|_| anyhow::anyhow!("Router unreachable from this Haarness server. Check the address and that the router is running."))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        if key.is_empty() {
            return Ok(None);
        }
        bail!("Router access key rejected. Check the key and try again.");
    }
    ensure!(
        response.status().is_success(),
        "Router returned HTTP {} while loading models",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Router connection interrupted while loading models")?
    {
        ensure!(
            bytes.len() + chunk.len() <= 4 * 1024 * 1024,
            "Router model catalog is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let body: serde_json::Value = serde_json::from_slice(&bytes)
        .context("The address did not return a router model catalog")?;
    let models = body["data"]
        .as_array()
        .context("The address did not return a router model catalog")?;
    ensure!(
        models
            .iter()
            .all(|model| model["id"].as_str().is_some_and(|id| !id.is_empty())),
        "Router returned an invalid model catalog"
    );
    Ok(Some(models.len()))
}

pub(crate) async fn connect(
    paths: &crate::Paths,
    workspace: &Path,
    trust: bool,
    request: &ConnectRequest,
) -> Result<ConnectResult> {
    ensure!(
        !request.project || trust,
        "Trust this project before adding a project router connection"
    );
    let url = normalize_url(&request.url)?;
    // Probe anonymously first: a key is optional unless the router asks for it.
    let (models, requires_auth) = match probe(&url, "").await? {
        Some(models) => (models, false),
        None if request.key.trim().is_empty() => return Ok(ConnectResult::NeedsKey),
        None => (
            probe(&url, request.key.trim())
                .await?
                .context("Router access key required")?,
            true,
        ),
    };
    let _guard = WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current = crate::load_config(paths, workspace, trust)?;
    let store = CredentialStore::new(paths.auth.clone());
    let mut suffix = 1;
    let id = loop {
        let id = if suffix == 1 {
            "router".into()
        } else {
            format!("router_{suffix}")
        };
        if !current.providers.contains_key(&id) && store.get(&id)?.is_none() {
            break id;
        }
        suffix += 1;
    };
    let name = if request.name.trim().is_empty() {
        "Haarness Router"
    } else {
        request.name.trim()
    }
    .to_owned();
    let section = crate::ProviderSection {
        integration: Some(Integration::HaarnessRouter {
            deployment: RouterDeployment::SelfHosted,
        }),
        name: Some(name.clone()),
        base_url: Some(url),
        requires_auth,
        // An empty model list enables dynamic discovery; never freeze a live catalog.
        models: Vec::new(),
        ..crate::ProviderSection::default()
    };
    if requires_auth {
        store.set(&id, &Credential::api(request.key.trim().to_owned()))?;
    }
    let target = crate::provider_config_target(paths, workspace, request.project);
    if let Err(error) = crate::write_provider_entry(&target, &id, Some(&section)) {
        if requires_auth {
            let _ = store.remove(&id);
        }
        return Err(error);
    }
    Ok(ConnectResult::Connected { id, name, models })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_preserve_proxy_paths_and_normalize_the_api_suffix() {
        for (input, expected) in [
            ("localhost:8080", "http://localhost:8080/v1"),
            (
                " https://router.example/team/ ",
                "https://router.example/team/v1",
            ),
            ("http://192.168.1.2:8080/v1/", "http://192.168.1.2:8080/v1"),
        ] {
            assert_eq!(normalize_url(input).unwrap(), expected);
        }
        for input in [
            "",
            "file:///tmp/router",
            "https://user:key@example.com",
            "https://router.example?key=secret",
            "https://router.example/#key",
        ] {
            assert!(normalize_url(input).is_err());
        }
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
    };

    async fn mock(key_required: bool) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/v1/models",
            get(move |headers: HeaderMap| async move {
                if key_required
                    && headers.get("authorization").and_then(|v| v.to_str().ok())
                        != Some("Bearer fixture-key")
                {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(serde_json::json!({"data":[{"id":"openai/new-model"},{"id":"fast"}]}))
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, task)
    }

    #[tokio::test]
    async fn connects_without_keys_keeps_models_dynamic_and_assigns_stable_unique_ids() {
        let root =
            std::env::temp_dir().join(format!("haarness-router-connect-{}", uuid::Uuid::now_v7()));
        let paths = crate::Paths::new(Some(&root)).unwrap();
        let (url, task) = mock(false).await;
        let request = ConnectRequest {
            url,
            name: "Team router".into(),
            key: String::new(),
            project: false,
        };
        for expected in ["router", "router_2"] {
            let ConnectResult::Connected { id, models, .. } =
                connect(&paths, &root, false, &request).await.unwrap()
            else {
                panic!("unexpected key prompt")
            };
            assert_eq!(id, expected);
            assert_eq!(models, 2);
        }
        let config = crate::load_config(&paths, &root, false).unwrap();
        assert_eq!(
            config.providers["router"].name.as_deref(),
            Some("Team router")
        );
        assert!(!config.providers["router"].requires_auth);
        assert!(config.providers["router"].models.is_empty());
        assert_eq!(
            config.providers["router"].integration,
            Some(Integration::HaarnessRouter {
                deployment: RouterDeployment::SelfHosted
            })
        );
        assert!(
            CredentialStore::new(paths.auth.clone())
                .all()
                .unwrap()
                .is_empty()
        );
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_router_without_provider_keys_stays_connected_and_says_what_it_needs() {
        let root =
            std::env::temp_dir().join(format!("haarness-router-empty-{}", uuid::Uuid::now_v7()));
        let paths = crate::Paths::new(Some(&root)).unwrap();
        let app = Router::new().route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({"object":"list","data":[]})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let request = ConnectRequest {
            url,
            name: String::new(),
            key: String::new(),
            project: false,
        };
        let ConnectResult::Connected { id, models, .. } =
            connect(&paths, &root, false, &request).await.unwrap()
        else {
            panic!("unexpected key prompt")
        };
        assert_eq!(models, 0);
        // Kept, so the router's models appear once it has a key.
        assert!(
            crate::load_config(&paths, &root, false)
                .unwrap()
                .providers
                .contains_key(&id)
        );
        assert!(NO_MODELS_YET.contains("restart"));
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn prompts_only_when_required_and_never_saves_a_rejected_connection_or_key() {
        let root =
            std::env::temp_dir().join(format!("haarness-router-auth-{}", uuid::Uuid::now_v7()));
        let paths = crate::Paths::new(Some(&root)).unwrap();
        let (url, task) = mock(true).await;
        let mut request = ConnectRequest {
            url,
            name: String::new(),
            key: String::new(),
            project: false,
        };
        assert!(matches!(
            connect(&paths, &root, false, &request).await.unwrap(),
            ConnectResult::NeedsKey
        ));
        request.key = "rejected-fixture-secret".into();
        let error = connect(&paths, &root, false, &request)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("key rejected"));
        assert!(!error.contains(&request.key));
        assert!(
            crate::load_config(&paths, &root, false)
                .unwrap()
                .providers
                .is_empty()
        );
        assert!(
            CredentialStore::new(paths.auth.clone())
                .all()
                .unwrap()
                .is_empty()
        );
        request.key = "fixture-key".into();
        let ConnectResult::Connected { id, .. } =
            connect(&paths, &root, false, &request).await.unwrap()
        else {
            panic!("expected connection")
        };
        let config = crate::load_config(&paths, &root, false).unwrap();
        assert!(config.providers[&id].requires_auth);
        assert!(
            !std::fs::read_to_string(crate::provider_config_target(&paths, &root, false))
                .unwrap()
                .contains("fixture-key")
        );
        assert!(
            CredentialStore::new(paths.auth.clone())
                .get(&id)
                .unwrap()
                .is_some()
        );
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_redirects_and_invalid_catalogs_before_saving() {
        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                Response::builder()
                    .status(302)
                    .header("location", "http://127.0.0.1:1/keys")
                    .body(axum::body::Body::empty())
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        assert!(
            probe(&url, "fixture-key")
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("302")
        );
        task.abort();
    }
}
