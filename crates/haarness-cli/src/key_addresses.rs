//! Saved provider keys and the addresses they are for.
//!
//! A key is saved in the person's `auth.json` under a provider id, whichever
//! configuration defines that id, and a project's configuration may define a
//! provider under any id and at any address. So a provider that only a
//! project defines is sent a saved key only when the key was saved for its
//! address (scheme, host, port and path, see [`key_address`]) or one above it
//! on the same server, and follows no redirect off that server; the providers
//! Haarness ships and those the person's own configuration defines send their
//! keys as they always have. Connecting a provider saves its key for the
//! address its configuration gives it, and connecting it again where a project
//! points it elsewhere saves the key for that address too, so nothing ever
//! asks.
//!
//! A key an earlier Haarness saved has no address recorded. It gets the
//! addresses of the definitions that used it before, once, from the person's
//! global configuration and the projects registered before this Haarness
//! first ran with these data (see [`record_earlier_keys`]): a project opened
//! for the first time since then is not among them.
//!
//! A chat on such a provider whose saved key is for another address, and that
//! has no key of its own, fails with the notice before anything is sent.
//!
//! A provider a project defines with Vertex AI's, Amazon Bedrock's or Azure's
//! protocol at an address that is not that cloud's own is also never given
//! this computer's sign-in to the cloud (see
//! [`haarness_providers::own_cloud_address`]).

use crate::{FileConfig, Paths, ProviderSection};
use anyhow::Result;
use chrono::{DateTime, Utc};
use haarness_providers::{
    CompatibleProtocol, CredentialFence,
    auth::{Credential, CredentialStore, key_address},
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};

/// Where the time this Haarness first ran with a data folder is kept.
const FIRST_RUN: &str = "key-addresses.json";

/// What the providers of one configuration are kept from, and what the person
/// is told about it.
#[derive(Debug, Default)]
pub(crate) struct KeyFence {
    project: BTreeSet<String>,
    /// Providers whose saved credential is not sent, with the notice.
    withheld: BTreeMap<String, String>,
    /// The same providers, with why (the notice's first sentence).
    reasons: BTreeMap<String, String>,
    /// Those of `withheld` with no key of their own: a chat on one fails
    /// with the notice before it is sent.
    refused: BTreeSet<String>,
    /// Providers that may not use this computer's cloud sign-in, with the
    /// notice when they would have used it.
    no_ambient: BTreeMap<String, Option<String>>,
}

impl KeyFence {
    /// The part the provider registry applies.
    pub(crate) fn providers(&self) -> CredentialFence {
        CredentialFence {
            project: self.project.iter().cloned().collect(),
            no_ambient: self.no_ambient.keys().cloned().collect(),
            withheld: self
                .refused
                .iter()
                .filter_map(|id| Some((id.clone(), self.withheld.get(id)?.clone())))
                .collect(),
        }
    }

    /// Why each provider's saved key is kept from it, as a command that
    /// refuses it says so.
    pub(crate) fn reasons(&self) -> &BTreeMap<String, String> {
        &self.reasons
    }

    /// Whether `id`'s saved credential is kept from it.
    pub(crate) fn withholds(&self, id: &str) -> bool {
        self.withheld.contains_key(id)
    }

    /// Whether `id` may not use this computer's cloud sign-in.
    pub(crate) fn without_ambient(&self, id: &str) -> bool {
        self.no_ambient.contains_key(id)
    }

    /// What the person is told about `id`, when anything.
    pub(crate) fn notice(&self, id: &str) -> Option<&str> {
        self.withheld
            .get(id)
            .map(String::as_str)
            .or_else(|| self.no_ambient.get(id).and_then(Option::as_deref))
    }

    /// Every notice, by provider id, as the log and `haarness providers`
    /// say it, with both addresses: `Provider <id>: the saved key …`.
    pub(crate) fn log_lines(&self) -> Vec<String> {
        self.notices()
            .map(|(id, notice)| match self.reasons.get(id) {
                Some(reason) => format!(
                    "Provider {id}: {} Connect it in this project to save a key for that address.",
                    lower_first(reason)
                ),
                None => format!("Provider {id}: {}", lower_first(notice)),
            })
            .collect()
    }

    /// Every notice, by provider id.
    pub(crate) fn notices(&self) -> impl Iterator<Item = (&str, &str)> {
        self.withheld
            .keys()
            .chain(self.no_ambient.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| Some((id.as_str(), self.notice(id)?)))
    }
}

/// `text` with its first letter in lower case, to follow a colon.
pub(crate) fn lower_first(text: &str) -> String {
    let mut letters = text.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(letters).collect()
    })
}

/// `config`'s fence, given the saved records `auth`.
pub(crate) fn fence(config: &FileConfig, auth: &BTreeMap<String, Value>) -> KeyFence {
    let mut fence = KeyFence::default();
    for id in &config.project_providers {
        let Some(section) = config.providers.get(id).filter(|section| section.enabled) else {
            continue;
        };
        let Some(url) = section.base_url.as_deref() else {
            continue;
        };
        fence.project.insert(id.clone());
        let address = key_address(url).unwrap_or_else(|| url.to_owned());
        let credential = auth
            .get(id)
            .and_then(|raw| serde_json::from_value::<Credential>(raw.clone()).ok());
        // A key of its own, or a saved one (sent or not), is what it uses
        // instead of a cloud sign-in.
        let own = own_key(section);
        let mut keyed = own;
        if let Some(credential) = credential {
            keyed = true;
            if !credential.saved_for(url) {
                let (reason, notice) = withheld_words(&address, &credential);
                fence.reasons.insert(id.clone(), reason);
                fence.withheld.insert(id.clone(), notice);
                if !own && section.headers.is_empty() && section.requires_auth {
                    fence.refused.insert(id.clone());
                }
            }
        }
        let Ok(protocol) = CompatibleProtocol::parse(&section.protocol) else {
            continue;
        };
        let (cloud, owner) = match protocol {
            CompatibleProtocol::GoogleVertex => ("Google Cloud", "Google's"),
            CompatibleProtocol::AmazonBedrock => ("AWS", "Amazon Bedrock's"),
            CompatibleProtocol::AzureOpenAi => ("Azure", "Azure's"),
            _ => continue,
        };
        if !haarness_providers::own_cloud_address(protocol, url) {
            fence.no_ambient.insert(
                id.clone(),
                (!keyed).then(|| {
                    format!(
                        "Your {cloud} sign-in goes only to {owner} addresses. Define this provider in your global configuration to use it at {address}."
                    )
                }),
            );
        }
    }
    fence
}

/// Whether the definition names a key of its own in the environment.
fn own_key(section: &ProviderSection) -> bool {
    section
        .api_key_env
        .iter()
        .chain(&section.api_key_envs)
        .any(|variable| std::env::var_os(variable).is_some_and(|value| !value.is_empty()))
}

/// Why a saved key was not sent to `address`, with both addresses (for the
/// log and `haarness run`), and the notice where the provider is connected
/// or chatted with: short enough for two rows of the terminal's key prompt
/// at 80 columns, the address last, so a long one is what a row cuts.
fn withheld_words(address: &str, credential: &Credential) -> (String, String) {
    let connect = format!("Connect to save a key for {address}.");
    if credential.addresses_pending() {
        return (
            format!(
                "The saved key's addresses are still being looked for, so it was not sent to {address} yet."
            ),
            format!("Still checking where the saved key was used. {connect}"),
        );
    }
    let reason = match credential
        .addresses()
        .filter(|addresses| !addresses.is_empty())
    {
        Some(addresses) => format!(
            "The saved key is for {}, so it was not sent to {address}.",
            addresses.join(", ")
        ),
        None => format!("The saved key is not for {address}, so it was not sent there."),
    };
    (
        reason,
        format!("The saved key is for another address. {connect}"),
    )
}

/// The saved records, after the addresses of keys an earlier Haarness saved
/// are recorded, and `config`'s fence, for `workspace`.
pub(crate) fn current(
    paths: &Paths,
    workspace: &Path,
    config: &FileConfig,
) -> Result<(BTreeMap<String, Value>, KeyFence)> {
    let auth = records(paths, workspace)?;
    let fence = fence(config, &auth);
    Ok((auth, fence))
}

/// [`current`], or no saved records when they cannot be read (said in the
/// log): the providers a project defines are fenced all the same.
pub(crate) fn current_or_none(
    paths: &Paths,
    workspace: &Path,
    config: &FileConfig,
) -> (BTreeMap<String, Value>, KeyFence) {
    current(paths, workspace, config).unwrap_or_else(|error| {
        tracing::warn!(%error, "config: saved provider keys");
        (BTreeMap::new(), fence(config, &BTreeMap::new()))
    })
}

/// The saved records, after the addresses of keys an earlier Haarness saved
/// are recorded: at once those `workspace`'s own definitions give (so the
/// project being opened never waits on the others), then those of every
/// project registered before, which a search finds on its own (see
/// [`earlier_addresses`]). Were they not saved, they still apply here.
pub(crate) fn records(paths: &Paths, workspace: &Path) -> Result<BTreeMap<String, Value>> {
    let store = CredentialStore::new(paths.auth.clone());
    let mut auth = store.all_raw()?;
    let pending = earlier_keys(&auth);
    if pending.is_empty() {
        return Ok(auth);
    }
    let here = addresses_here(paths, workspace, &pending);
    if let Err(error) = store.record_addresses(&here, false) {
        tracing::warn!(%error, "could not record the addresses of saved keys");
    }
    let found = earlier_addresses(paths, &pending);
    if let Ok(saved) = store.all_raw() {
        auth = saved;
    }
    merge(&mut auth, &here, false);
    if let Some(found) = found {
        merge(&mut auth, &found, true);
    }
    Ok(auth)
}

/// What [`CredentialStore::record_addresses`] makes of `auth`, in memory.
fn merge(auth: &mut BTreeMap<String, Value>, found: &Found, done: bool) {
    for (id, (looked_for, addresses)) in found {
        let Some(credential) = auth
            .get(id)
            .and_then(|raw| serde_json::from_value::<Credential>(raw.clone()).ok())
            .filter(|credential| {
                credential.api_key() == Some(looked_for.as_str()) && credential.addresses_pending()
            })
        else {
            continue;
        };
        let mut all = credential
            .addresses()
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        for address in addresses {
            if !all.contains(address) {
                all.push(address.clone());
            }
        }
        if let Ok(value) =
            serde_json::to_value(Credential::api_found(looked_for.clone(), all, !done))
        {
            auth.insert(id.clone(), value);
        }
    }
}

/// How long the first look for the addresses of keys saved before may hold a
/// process (a server's start, a command); it goes on after that, and its
/// result reaches a server as the change to the key file it makes.
const SEARCH_PATIENCE: Duration = Duration::from_secs(2);

/// By provider id: the key looked for, and the addresses found for it.
type Found = BTreeMap<String, (String, Vec<String>)>;

/// The addresses [`record_earlier_keys`] finds for `pending`, searched on a
/// thread of its own, so a project folder on a network volume that hangs
/// holds nothing up, and once a process for each set of keys, so keys it
/// could not record are not searched for again and again. Only the first
/// call waits for it. `None` while the search runs on.
fn earlier_addresses(paths: &Paths, pending: &BTreeMap<String, String>) -> Option<Found> {
    let key = format!(
        "{}\n{}",
        paths.auth.display(),
        pending.keys().cloned().collect::<Vec<_>>().join("\n")
    );
    let (paths, pending) = (paths.clone(), pending.clone());
    once_within(key, SEARCH_PATIENCE, move || {
        record_earlier_keys(&paths, &pending).unwrap_or_else(|error| {
            tracing::warn!(%error, "could not record the addresses of saved keys");
            Found::new()
        })
    })
}

#[derive(Default)]
struct Search {
    found: parking_lot::Mutex<Option<Found>>,
    done: parking_lot::Condvar,
    waited: std::sync::atomic::AtomicBool,
}

/// Runs `work` once for `key` in this process, on a thread of its own; the
/// first call waits up to `patience` for what it found, later ones take what
/// there is.
fn once_within(
    key: String,
    patience: Duration,
    work: impl FnOnce() -> Found + Send + 'static,
) -> Option<Found> {
    static SEARCHES: LazyLock<parking_lot::Mutex<HashMap<String, Arc<Search>>>> =
        LazyLock::new(Default::default);
    let (search, new) = {
        let mut searches = SEARCHES.lock();
        match searches.get(&key) {
            Some(search) => (search.clone(), false),
            None => {
                let search = Arc::new(Search::default());
                searches.insert(key, search.clone());
                (search, true)
            }
        }
    };
    if new {
        let running = search.clone();
        let started = std::thread::Builder::new()
            .name("saved-key-addresses".into())
            .spawn(move || {
                let found = work();
                *running.found.lock() = Some(found);
                running.done.notify_all();
            });
        if let Err(error) = started {
            tracing::warn!(%error, "could not search for the addresses of saved keys");
            *search.found.lock() = Some(Found::new());
        }
    }
    let mut found = search.found.lock();
    if !search
        .waited
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        let _ = search
            .done
            .wait_while_for(&mut found, |found| found.is_none(), patience);
    }
    found.clone()
}

/// The API keys, by provider id, whose addresses are not all known: saved by
/// an earlier Haarness, before it recorded any, and not yet looked for to the
/// end. The providers Haarness ships keep their keys at their own addresses,
/// so theirs need none.
fn earlier_keys(auth: &BTreeMap<String, Value>) -> BTreeMap<String, String> {
    auth.iter()
        .filter(|(id, _)| !haarness_providers::builtin_provider(id))
        .filter_map(|(id, raw)| {
            let credential = serde_json::from_value::<Credential>(raw.clone()).ok()?;
            let key = credential
                .api_key()
                .filter(|_| credential.addresses_pending())?;
            Some((id.clone(), key.to_owned()))
        })
        .collect()
}

/// The addresses `workspace`'s own definitions give `pending`'s keys, read
/// at once: the person's global definition of an id when there is one, else
/// the project's, when it was registered before this Haarness first ran with
/// these data and its files have not changed since.
fn addresses_here(paths: &Paths, workspace: &Path, pending: &BTreeMap<String, String>) -> Found {
    let since = first_run(paths);
    let global = provider_entries(
        &crate::scoped_config_files(paths, Path::new("."), false),
        None,
    );
    // Registered paths are stored resolved; only this one is resolved here,
    // so a folder on a volume that does not answer is never touched.
    let here = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let same = |path: &str| Path::new(path) == here;
    let before = paths
        .workspace_manager()
        .and_then(|manager| Ok(manager.registered()?))
        .is_ok_and(|registered| {
            registered
                .iter()
                .any(|entry| !entry.chat_home && entry.created_at < since && same(&entry.path))
        });
    let project = if before {
        provider_entries(
            &crate::scoped_config_files(paths, workspace, true),
            Some(since),
        )
    } else {
        BTreeMap::new()
    };
    pending
        .iter()
        .filter_map(|(id, key)| {
            let urls = global.get(id).or_else(|| project.get(id))?;
            let addresses = urls.iter().filter_map(|url| key_address(url)).collect();
            Some((id.clone(), (key.clone(), addresses)))
        })
        .collect()
}

/// Record, for each of `pending`'s keys, the addresses the definitions that
/// could use it before give its provider: the person's global definition of
/// the id when there is one (a project could not move it), else the
/// definitions of the projects registered before this Haarness first ran with
/// these data, in files last changed before then (not ones a `git pull` has
/// brought since). A key no such definition names is saved for no address in
/// particular, so only the person's own definitions use it.
fn record_earlier_keys(paths: &Paths, pending: &BTreeMap<String, String>) -> Result<Found> {
    let since = first_run(paths);
    let mut found: BTreeMap<String, BTreeSet<String>> = pending
        .keys()
        .map(|id| (id.clone(), BTreeSet::new()))
        .collect();
    let global = provider_entries(
        &crate::scoped_config_files(paths, Path::new("."), false),
        None,
    );
    for workspace in paths.workspace_manager()?.registered()? {
        if workspace.chat_home || workspace.created_at >= since {
            continue;
        }
        let folder = PathBuf::from(&workspace.path);
        if !folder.is_dir() {
            continue;
        }
        let files = crate::scoped_config_files(paths, &folder, true);
        for (id, urls) in provider_entries(&files, Some(since)) {
            if global.contains_key(&id) {
                continue;
            }
            if let Some(addresses) = found.get_mut(&id) {
                addresses.extend(urls.iter().filter_map(|url| key_address(url)));
            }
        }
    }
    for (id, urls) in &global {
        if let Some(addresses) = found.get_mut(id) {
            *addresses = urls.iter().filter_map(|url| key_address(url)).collect();
        }
    }
    let found: Found = found
        .into_iter()
        .filter_map(|(id, addresses)| {
            let key = pending.get(&id)?.clone();
            Some((id, (key, addresses.into_iter().collect())))
        })
        .collect();
    CredentialStore::new(paths.auth.clone()).record_addresses(&found, true)?;
    for (id, (_, addresses)) in &found {
        tracing::info!(provider = %id, ?addresses, "recorded the addresses of a key saved before");
    }
    Ok(found)
}

/// Each provider entry of `files`, by id, with the address it names unless it
/// is turned off. With `before`, only files last changed before then count.
fn provider_entries(
    files: &[PathBuf],
    before: Option<DateTime<Utc>>,
) -> BTreeMap<String, Vec<String>> {
    let mut entries: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in files {
        if let Some(before) = before {
            let changed = fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .map(DateTime::<Utc>::from);
            if !changed.is_ok_and(|changed| changed < before) {
                continue;
            }
        }
        let root = if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            fs::read_to_string(path)
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                .and_then(|value| serde_json::to_value(value).ok())
        } else {
            crate::jsonconfig::read_json(path).ok().flatten()
        };
        let Some(providers) = root
            .as_ref()
            .and_then(|root| root.get("providers"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (id, entry) in providers {
            let urls = entries.entry(id.clone()).or_default();
            if entry.get("enabled").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            if let Some(url) = entry.get("base_url").and_then(Value::as_str) {
                urls.push(url.to_owned());
            }
        }
    }
    entries
}

/// Note when this Haarness first runs with `paths`' data. Called before it
/// registers any project, so a project first opened now is not taken for one
/// that used a saved key before.
pub(crate) fn note_first_run(paths: &Paths) {
    let _ = first_run(paths);
}

fn first_run(paths: &Paths) -> DateTime<Utc> {
    let path = paths.data.join(FIRST_RUN);
    let read = || {
        let text = fs::read_to_string(&path).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        DateTime::parse_from_rfc3339(value.get("firstRun")?.as_str()?)
            .ok()
            .map(|time| time.with_timezone(&Utc))
    };
    if let Some(time) = read() {
        return time;
    }
    let now = Utc::now();
    // Two processes starting at once keep the time of the one that wrote it.
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            let _ = file.write_all(
                serde_json::json!({ "firstRun": now.to_rfc3339() })
                    .to_string()
                    .as_bytes(),
            );
            now
        }
        Err(_) => read().unwrap_or(now),
    }
}

/// Save `key` for provider `id`, for the address `workspace`'s configuration
/// gives it (none for a provider with no address of its own, such as one
/// Haarness ships).
pub(crate) fn save_key(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    id: &str,
    key: &str,
) -> Result<()> {
    // A key saved before keeps the addresses it is used at when it is saved
    // again unchanged.
    let _ = records(paths, workspace);
    let config = crate::load_config(paths, workspace, trust).ok();
    let url = config.as_ref().and_then(|config| address(config, id));
    CredentialStore::new(paths.auth.clone()).set_api_key(id, key, url)?;
    Ok(())
}

/// The address `config` gives provider `id`.
pub(crate) fn address<'a>(config: &'a FileConfig, id: &str) -> Option<&'a str> {
    config
        .providers
        .get(id)
        .filter(|section| section.enabled)
        .and_then(|section| section.base_url.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::HeaderMap, routing::any};
    use haarness_providers::NativeProviderRegistry;
    use std::sync::Arc;

    /// What one fixture server was asked: method, path, `Authorization`.
    type Seen = Arc<parking_lot::Mutex<Vec<(String, String, Option<String>)>>>;

    /// A Chat Completions server that lists one model and answers every chat.
    async fn server() -> (String, Seen) {
        let seen: Seen = Arc::default();
        let record = seen.clone();
        let app = Router::new().fallback(any(
            move |method: axum::http::Method, uri: axum::http::Uri, headers: HeaderMap| {
                let record = record.clone();
                async move {
                    record.lock().push((
                        method.to_string(),
                        uri.path().to_owned(),
                        headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                    ));
                    if uri.path().ends_with("/models") {
                        return (
                            [(axum::http::header::CONTENT_TYPE, "application/json")],
                            r#"{"data":[{"id":"listed"}]}"#.to_owned(),
                        );
                    }
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_owned(),
                    )
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (address, seen)
    }

    fn root(name: &str) -> (PathBuf, Paths) {
        let root = std::env::temp_dir().join(format!("haarness-{name}-{}", uuid::Uuid::now_v7()));
        let paths = Paths::new(Some(&root)).unwrap();
        (root, paths)
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// The registry a server builds for `workspace`, its local runtimes left
    /// out, and the fence it used.
    async fn providers(paths: &Paths, workspace: &Path) -> (NativeProviderRegistry, KeyFence) {
        let config = crate::load_config(paths, workspace, true).unwrap();
        let (auth, fence) = current(paths, workspace, &config).unwrap();
        let credentials = crate::resolved_credentials(&auth);
        let (instances, mut disabled) = crate::provider_instances(&config, &credentials, &fence);
        disabled.extend(["ollama".to_owned(), "lmstudio".to_owned()]);
        let registry = NativeProviderRegistry::with_providers_and_store(
            instances,
            &disabled,
            Arc::new(CredentialStore::new(paths.auth.clone())),
            &fence.providers(),
        )
        .await;
        (registry, fence)
    }

    /// Lists `id`'s models as a server does once it answers, then sends it a
    /// chat.
    async fn use_provider(registry: &NativeProviderRegistry, id: &str) {
        let provider = haarness_core::ProviderRegistry::get(registry, id).unwrap();
        provider.refresh_models(true).await;
        let _ = chat(registry, id).await;
    }

    /// Sends `id` a chat; what failed, as the chat shows it.
    async fn chat(registry: &NativeProviderRegistry, id: &str) -> std::result::Result<(), String> {
        use futures::StreamExt as _;
        let provider = haarness_core::ProviderRegistry::get(registry, id).unwrap();
        let request = haarness_core::ProviderRequest {
            model: "listed".into(),
            system: String::new(),
            messages: vec![haarness_core::ProviderMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        };
        let stream = provider
            .stream(request, tokio_util::sync::CancellationToken::new())
            .await
            .map_err(|error| error.to_string())?;
        let _ = stream.collect::<Vec<_>>().await;
        Ok(())
    }

    /// A key saved for the router at A is not sent to B, where a project
    /// defines the same id: no model list is asked for and a chat carries no
    /// key. Connected again in that project, the key is saved for B too. A
    /// provider the person's own configuration defines is unaffected.
    #[tokio::test]
    async fn a_project_cannot_point_a_saved_key_at_another_address() {
        let (a, seen_a) = server().await;
        let (b, seen_b) = server().await;
        let (root, paths) = root("key-fence");
        let workspace = root.join("cloned");
        write(
            &paths.config_dir().join("haarness.json"),
            &format!(r#"{{"providers":{{"company":{{"base_url":"{b}/v1"}}}}}}"#),
        );
        write(
            &workspace.join(".haarness/haarness.json"),
            &format!(
                r#"{{"providers":{{"router":{{"name":"Team router","base_url":"{b}/v1"}}}}}}"#
            ),
        );
        let store = CredentialStore::new(paths.auth.clone());
        store
            .set_api_key("router", "router-key", Some(&format!("{a}/v1")))
            .unwrap();
        store
            .set_api_key("company", "company-key", Some(&format!("{a}/v1")))
            .unwrap();

        let (registry, fence) = providers(&paths, &workspace).await;
        assert!(fence.withholds("router"));
        assert!(!fence.withholds("company"), "the person's own definition");
        let notice = fence.notice("router").unwrap();
        assert_eq!(
            notice,
            format!("The saved key is for another address. Connect to save a key for {b}/v1.")
        );
        assert_eq!(
            fence.log_lines(),
            [format!(
                "Provider router: the saved key is for {a}/v1, so it was not sent to {b}/v1. Connect it in this project to save a key for that address."
            )]
        );
        assert_eq!(
            fence.providers().withheld.get("router").map(String::as_str),
            Some(notice)
        );
        // A chat on it says so before anything is sent.
        let refused = chat(&registry, "router").await.unwrap_err();
        assert!(refused.ends_with(notice), "{refused}");
        use_provider(&registry, "router").await;
        use_provider(&registry, "company").await;
        let asked = seen_b.lock().clone();
        // The router's key never reached B, and B was asked for no model
        // list but the company's own.
        assert!(
            asked
                .iter()
                .all(|(_, _, key)| key.as_deref() != Some("Bearer router-key")),
            "{asked:?}"
        );
        assert!(
            asked
                .iter()
                .filter(|(method, path, _)| method == "GET" && path == "/v1/models")
                .all(|(_, _, key)| key.as_deref() == Some("Bearer company-key")),
            "{asked:?}"
        );
        assert!(asked.contains(&(
            "GET".into(),
            "/v1/models".into(),
            Some("Bearer company-key".into())
        )));
        assert!(
            !asked.iter().any(|(_, _, key)| key.is_none()),
            "nothing was sent without a key: {asked:?}"
        );
        assert!(seen_a.lock().is_empty());

        // Connected again where the project points it.
        seen_b.lock().clear();
        save_key(&paths, &workspace, true, "router", "router-key").unwrap();
        let (registry, fence) = providers(&paths, &workspace).await;
        assert!(fence.notice("router").is_none());
        use_provider(&registry, "router").await;
        assert!(seen_b.lock().contains(&(
            "GET".into(),
            "/v1/models".into(),
            Some("Bearer router-key".into())
        )));
        // The address it was saved for first still gets it.
        assert!(
            CredentialStore::new(paths.auth.clone())
                .for_address(&format!("{a}/v1"))
                .get("router")
                .unwrap()
                .is_some()
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Keys a 0.1.22 Haarness saved, with no address: each gets the
    /// addresses of the definitions that used it, so the person's projects
    /// keep working, while a project first opened after the update does not
    /// get them.
    #[tokio::test]
    async fn keys_saved_before_keep_working_where_they_were_used() {
        let (router, seen_router) = server().await;
        let (team, _) = server().await;
        let (evil, seen_evil) = server().await;
        let (root, paths) = root("key-upgrade");
        write(
            &paths.auth,
            r#"{"router":{"type":"api","key":"router-key"},"team":"team-key","anthropic":{"type":"api","key":"sk-ant"},"spare":{"type":"api","key":"spare-key"},"company":{"type":"api","key":"company-key"}}"#,
        );
        write(
            &paths.config_dir().join("haarness.json"),
            r#"{"providers":{"company":{"base_url":"https://models.example.com/v1"}}}"#,
        );
        // Project A, set up through Desktop: a router connected for this
        // project and a custom provider added for it.
        let project = root.join("project-a");
        write(
            &project.join("haarness.json"),
            &format!(
                r#"{{"providers":{{"router":{{"integration":{{"kind":"haarness-router","deployment":"self-hosted"}},"name":"Haarness Router","base_url":"{router}/v1","requires_auth":true}},"team":{{"protocol":"openai-chat-completions","name":"Team","base_url":"{team}/v1"}},"company":{{"base_url":"{evil}/v1"}}}}}}"#
            ),
        );
        let manager = paths.workspace_manager().unwrap();
        manager.register_local(&project).unwrap();
        let pulled = root.join("pulled");
        fs::create_dir_all(&pulled).unwrap();
        manager.register_local(&pulled).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // The updated Haarness runs; then a `git pull` brings a definition
        // into a project opened before, and a cloned project is opened.
        note_first_run(&paths);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(
            &pulled.join("haarness.json"),
            &format!(r#"{{"providers":{{"spare":{{"base_url":"{evil}/v1"}}}}}}"#),
        );
        let cloned = root.join("cloned");
        write(
            &cloned.join(".haarness/haarness.json"),
            &format!(
                r#"{{"providers":{{"router":{{"base_url":"{evil}/v1"}},"spare":{{"base_url":"{evil}/v1"}}}}}}"#
            ),
        );
        manager.register_local(&cloned).unwrap();

        let (registry, fence) = providers(&paths, &project).await;
        assert!(fence.notices().next().is_none(), "{fence:?}");
        use_provider(&registry, "router").await;
        assert!(seen_router.lock().contains(&(
            "GET".into(),
            "/v1/models".into(),
            Some("Bearer router-key".into())
        )));
        let store = CredentialStore::new(paths.auth.clone());
        let addresses = |id: &str| {
            store
                .get(id)
                .unwrap()
                .unwrap()
                .addresses()
                .map(<[String]>::to_vec)
        };
        assert_eq!(addresses("router"), Some(vec![format!("{router}/v1")]));
        assert_eq!(addresses("team"), Some(vec![format!("{team}/v1")]));
        assert_eq!(
            store.get("team").unwrap().unwrap().api_key(),
            Some("team-key")
        );
        assert_eq!(addresses("spare"), Some(Vec::new()), "used nowhere");
        // The person's global definition decides, as it did.
        assert_eq!(
            addresses("company"),
            Some(vec!["https://models.example.com/v1".to_owned()])
        );
        assert_eq!(addresses("anthropic"), None, "a built-in provider's key");

        let (registry, fence) = providers(&paths, &cloned).await;
        assert!(fence.withholds("router") && fence.withholds("spare"));
        use_provider(&registry, "router").await;
        use_provider(&registry, "spare").await;
        assert!(
            seen_evil.lock().iter().all(|(_, _, key)| key.is_none()),
            "{:?}",
            seen_evil.lock()
        );
        // Recorded once: opening the cloned project changed nothing.
        assert_eq!(addresses("router"), Some(vec![format!("{router}/v1")]));
        fs::remove_dir_all(root).unwrap();
    }

    /// A project's Vertex, Bedrock or Azure provider away from that cloud's
    /// own addresses is kept from this computer's sign-in to it, and says so
    /// when it has no key of its own; the person's own definitions are not.
    #[test]
    fn a_project_cloud_provider_elsewhere_gets_no_cloud_sign_in() {
        let (root, paths) = root("key-fence-cloud");
        let workspace = root.join("cloned");
        write(
            &paths.config_dir().join("haarness.json"),
            r#"{"providers":{"mine":{"protocol":"google-vertex","base_url":"https://vertex-proxy.example.com/v1"}}}"#,
        );
        write(
            &workspace.join("haarness.json"),
            r#"{"providers":{
                "team-vertex":{"protocol":"google-vertex","base_url":"https://vertex.evil.example/v1"},
                "team-bedrock":{"protocol":"amazon-bedrock","base_url":"https://abc123.execute-api.us-east-1.amazonaws.com"},
                "team-azure":{"protocol":"azure-openai","base_url":"https://team.azure-api.net/openai/v1","api_key_env":"HOME"},
                "real-vertex":{"protocol":"google-vertex","base_url":"https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1/publishers/google"},
                "plain":{"base_url":"https://models.example.com/v1"}
            }}"#,
        );
        let config = crate::load_config(&paths, &workspace, true).unwrap();
        let (_, fence) = current(&paths, &workspace, &config).unwrap();
        for id in ["team-vertex", "team-bedrock", "team-azure"] {
            assert!(fence.without_ambient(id), "{id}");
        }
        for id in ["mine", "real-vertex", "plain"] {
            assert!(!fence.without_ambient(id), "{id}");
        }
        assert_eq!(
            fence.notice("team-vertex").unwrap(),
            "Your Google Cloud sign-in goes only to Google's addresses. Define this provider in your global configuration to use it at https://vertex.evil.example/v1."
        );
        assert!(
            fence
                .notice("team-bedrock")
                .unwrap()
                .starts_with("Your AWS sign-in goes only to Amazon Bedrock's addresses.")
        );
        assert!(
            fence.notice("team-azure").is_none(),
            "a key of its own: nothing to say"
        );
        let applied = fence.providers();
        assert!(applied.no_ambient.contains("team-vertex"));
        assert!(applied.project.contains("plain") && !applied.project.contains("mine"));
        fs::remove_dir_all(root).unwrap();
    }

    /// The search for the addresses of keys saved before holds a start for
    /// its patience at most, and runs once for a set of keys: a search still
    /// running, or done, is not started again.
    #[test]
    fn the_search_for_earlier_addresses_waits_a_while_and_runs_once() {
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let key = format!("search-{}", uuid::Uuid::now_v7());
        let slow = |runs: Arc<std::sync::atomic::AtomicUsize>| {
            move || {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(400));
                Found::from([(
                    "team".to_owned(),
                    (
                        "team-key".to_owned(),
                        vec!["https://team.example/v1".to_owned()],
                    ),
                )])
            }
        };
        let started = std::time::Instant::now();
        assert!(once_within(key.clone(), Duration::from_millis(50), slow(runs.clone())).is_none());
        assert!(started.elapsed() < Duration::from_millis(300));
        // Only the first call waits.
        let again = std::time::Instant::now();
        assert!(once_within(key.clone(), Duration::from_secs(5), slow(runs.clone())).is_none());
        assert!(again.elapsed() < Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(600));
        let found = once_within(key, Duration::ZERO, slow(runs.clone())).unwrap();
        assert_eq!(found["team"].1, ["https://team.example/v1"]);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A search held up by a project folder that does not answer (a FIFO
    /// stands in for a stalled network volume): each project registered
    /// before the update keeps its own key at once, a project opened since
    /// is told the search is still on, a Connect meanwhile keeps the key
    /// looked for, and once the folder answers every project that used the
    /// key has it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stalled_search_breaks_no_project_that_used_the_key() {
        let (a, _) = server().await;
        let (b, seen_b) = server().await;
        let (c, _) = server().await;
        let (root, paths) = root("key-stalled");
        write(&paths.auth, r#"{"team":{"type":"api","key":"team-key"}}"#);
        let manager = paths.workspace_manager().unwrap();
        let define = |name: &str, address: &str| {
            let project = root.join(name);
            write(
                &project.join(".haarness/haarness.json"),
                &format!(
                    r#"{{"providers":{{"team":{{"name":"Team","base_url":"{address}/v1"}}}}}}"#
                ),
            );
            project
        };
        let (project_a, project_b) = (define("project-a", &a), define("project-b", &b));
        let stalled = root.join("stalled");
        fs::create_dir_all(stalled.join(".haarness")).unwrap();
        let fifo = stalled.join(".haarness/haarness.json");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        for project in [&project_a, &project_b, &stalled] {
            manager.register_local(project).unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
        note_first_run(&paths);
        std::thread::sleep(Duration::from_millis(20));
        let project_c = define("project-c", &c);
        manager.register_local(&project_c).unwrap();

        let started = std::time::Instant::now();
        let (_, fence) = providers(&paths, &project_a).await;
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(!fence.withholds("team"), "{fence:?}");
        let started = std::time::Instant::now();
        let (registry, fence) = providers(&paths, &project_b).await;
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "only the first look waits"
        );
        assert!(!fence.withholds("team"), "{fence:?}");
        use_provider(&registry, "team").await;
        assert!(
            seen_b
                .lock()
                .iter()
                .any(|(_, _, key)| key.as_deref() == Some("Bearer team-key"))
        );
        let (_, fence) = providers(&paths, &project_c).await;
        assert_eq!(
            fence.notice("team").unwrap(),
            format!(
                "Still checking where the saved key was used. Connect to save a key for {c}/v1."
            )
        );
        // `haarness run` and the log say so in the same words.
        assert_eq!(
            fence.log_lines(),
            [format!(
                "Provider team: the saved key's addresses are still being looked for, so it was not sent to {c}/v1 yet. Connect it in this project to save a key for that address."
            )]
        );
        let team = haarness_protocol::ProviderInfo {
            id: "team".into(),
            name: "Team".into(),
            authenticated: false,
            models: Vec::new(),
        };
        assert_eq!(
            crate::not_connected_provider(&team, fence.reasons()),
            format!(
                "Team is not connected here: the saved key's addresses are still being looked for, so it was not sent to {c}/v1 yet. Connect it with `haarness auth login team` in this project, then run this again."
            )
        );
        // Connected again in A meanwhile, with the same key.
        save_key(&paths, &project_a, true, "team", "team-key").unwrap();
        let store = CredentialStore::new(paths.auth.clone());
        assert!(store.get("team").unwrap().unwrap().addresses_pending());

        // The folder answers; the search records what it found.
        let mut writer = fs::OpenOptions::new().write(true).open(&fifo).unwrap();
        writer.write_all(b"{}").unwrap();
        drop(writer);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while store.get("team").unwrap().unwrap().addresses_pending() {
            assert!(
                std::time::Instant::now() < deadline,
                "the search did not finish"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let saved = store.get("team").unwrap().unwrap();
        let addresses = saved.addresses().unwrap();
        assert!(
            addresses.contains(&format!("{a}/v1")) && addresses.contains(&format!("{b}/v1")),
            "{addresses:?}"
        );
        assert!(!addresses.contains(&format!("{c}/v1")));
        let (_, fence) = providers(&paths, &project_b).await;
        assert!(!fence.withholds("team"));
        let (_, fence) = providers(&paths, &project_c).await;
        assert_eq!(
            fence.notice("team").unwrap(),
            format!("The saved key is for another address. Connect to save a key for {c}/v1.")
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// An empty key file (as an interrupted copy leaves) reads as none: a
    /// server starts and reloads with it.
    #[test]
    fn an_empty_key_file_holds_no_keys() {
        let (root, paths) = root("key-empty");
        write(&paths.auth, "\n");
        let config = crate::load_config(&paths, &root, true).unwrap();
        let (auth, fence) = current(&paths, &root, &config).unwrap();
        assert!(auth.is_empty());
        assert!(fence.notices().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
