//! Finds the Haarness servers on this computer that Chrome may work with. It
//! only reads `<cache>/workspaces/*/runtime.json` records and asks each server
//! who it is: it never locks `server.lock` or `connect.lock`, never starts or
//! replaces a server and never deletes a file. Records outlive crashed servers
//! and ports get reused, so every record is checked against the live server.
use crate::{
    layout::Layout,
    reconnect::{Backoff, Link, Pacing},
    rendezvous::{RuntimeDiscovery, read_runtime_file},
};
use reqwest::Url;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::Instant;

/// One Haarness data root's planes, canonicalised where they exist (legacy
/// roots are symlinks into the platform folders).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DataRoot {
    pub(crate) data: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) cache: PathBuf,
    pub(crate) state: PathBuf,
}

impl DataRoot {
    fn single(root: &Path) -> Self {
        Self {
            data: root.join("data"),
            config: root.join("config"),
            cache: root.join("cache"),
            state: root.join("state"),
        }
    }

    fn from_layout(layout: Layout) -> Self {
        Self {
            data: layout.data,
            config: layout.config,
            cache: layout.cache,
            state: layout.state,
        }
    }

    fn canonical(self) -> Self {
        Self {
            data: canonical(&self.data),
            config: canonical(&self.config),
            cache: canonical(&self.cache),
            state: canonical(&self.state),
        }
    }

    pub(crate) fn planes(&self) -> [&Path; 4] {
        [&self.data, &self.config, &self.cache, &self.state]
    }
}

/// `path` without Windows' `\\?\` prefix when it names a drive path: the
/// form people, Chrome and `..` in a joined path all understand.
pub(crate) fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// `path` with links resolved, when it exists; as given otherwise.
pub(crate) fn canonical(path: &Path) -> PathBuf {
    plain(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

/// The root `Paths::new` would use with this environment, resolved without
/// creating or migrating anything.
pub(crate) fn environment_root(
    env: &dyn Fn(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> Option<DataRoot> {
    if let Some(root) = env("HAARNESS_DATA_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
    {
        return Some(DataRoot::single(&root));
    }
    let home = home?;
    let root = env("HAARNESS_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    Layout::resolve(home, root.as_deref(), |key| env(key).map(PathBuf::from))
        .ok()
        .map(DataRoot::from_layout)
}

/// Every root the bridge looks in: the one its environment names (as the
/// wrapper set it), the default one with no `HAARNESS_*` or `XDG_*` variables
/// (where Desktop started from the Dock keeps its servers), and
/// `HAARNESS_BROWSER_EXTRA_ROOTS`. The first is the bridge's own.
pub(crate) fn data_roots(
    env: &dyn Fn(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> Vec<DataRoot> {
    let mut roots = Vec::new();
    roots.extend(environment_root(env, home));
    if let Some(home) = home {
        roots.extend(
            Layout::resolve(home, None, |_| None)
                .ok()
                .map(DataRoot::from_layout),
        );
    }
    if let Some(extra) = env("HAARNESS_BROWSER_EXTRA_ROOTS") {
        roots.extend(
            std::env::split_paths(&extra)
                .filter(|root| root.is_absolute())
                .map(|root| DataRoot::single(&root)),
        );
    }
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .map(DataRoot::canonical)
        .filter(|root| seen.insert(root.cache.clone()))
        .collect()
}

/// The home folder, as `Paths::new` finds it.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
}

/// A server's record, before asking the server.
#[derive(Clone)]
pub(crate) struct Record {
    pub(crate) file: PathBuf,
    pub(crate) discovery: RuntimeDiscovery,
    pub(crate) base: Url,
}

/// Records under every cache root whose process is alive and whose URL is on
/// this computer. A record that cannot be read is skipped: it is being
/// replaced, or it is not Haarness's.
pub(crate) fn records(caches: &[PathBuf], alive: fn(u32) -> bool) -> Vec<Record> {
    let mut found = Vec::new();
    for cache in caches {
        let Ok(entries) = fs::read_dir(cache.join("workspaces")) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path().join("runtime.json");
            let Ok(discovery) = read_runtime_file(&file) else {
                continue;
            };
            if discovery.pid == 0 || !alive(discovery.pid) {
                continue;
            }
            let Some(base) = loopback_base(&discovery.url) else {
                continue;
            };
            found.push(Record {
                file,
                discovery,
                base,
            });
        }
    }
    found.sort_by(|left, right| left.file.cmp(&right.file));
    found
}

/// `http://<loopback>:<port>/` for a server URL on this computer. A server
/// listening on every address is reached on loopback; any other host is not
/// this computer's.
pub(crate) fn loopback_base(url: &str) -> Option<Url> {
    let mut url = Url::parse(url).ok()?;
    if url.scheme() != "http" || url.port().is_none() {
        return None;
    }
    let host = url.host_str()?;
    let host = match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(IpAddr::V4(ip)) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Ok(IpAddr::V6(ip)) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        Ok(ip) if ip.is_loopback() => ip,
        Err(_) if host.eq_ignore_ascii_case("localhost") => IpAddr::V4(Ipv4Addr::LOCALHOST),
        _ => return None,
    };
    url.set_ip_host(host).ok()?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

/// Whether a process may still own its record. A PID that no longer exists
/// cannot. Windows has no cheap probe; the HTTP check decides there.
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
        let Ok(raw) = i32::try_from(pid) else {
            return false;
        };
        !matches!(kill(Pid::from_raw(raw), None), Err(Errno::ESRCH))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// What `/v1/info` says, read leniently: older servers lack `pid`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    name: String,
    #[serde(default)]
    version: String,
    server_instance_id: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
}

/// A live Haarness server on this computer. Its URL and token never reach
/// the extension.
#[derive(Clone)]
pub(crate) struct Server {
    pub(crate) id: String,
    pub(crate) workspace: String,
    pub(crate) version: String,
    pub(crate) base: Url,
    pub(crate) token: String,
    /// Accepts an extension host; otherwise listed as needing a restart.
    pub(crate) capable: bool,
    /// The record it was found through. Its folder is the workspace id, and
    /// it says which executable started the server.
    pub(crate) record: std::sync::Arc<Record>,
}

impl Server {
    /// The project folder's name, for people.
    pub(crate) fn name(&self) -> String {
        Path::new(&self.workspace).file_name().map_or_else(
            || self.workspace.clone(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

enum Verdict {
    Live(Server),
    /// Not a Haarness server for this record: the wrong identity, another
    /// process on a reused port, or another folder.
    Rejected(String),
    /// Could not ask; worth asking again.
    Unreachable(String),
}

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("a plain HTTP client always builds")
}

const INFO_TIMEOUT: Duration = Duration::from_millis(750);

async fn verify(http: &reqwest::Client, record: &Record) -> Verdict {
    let Ok(url) = record.base.join("v1/info") else {
        return Verdict::Rejected("invalid URL".into());
    };
    let response = tokio::time::timeout(
        INFO_TIMEOUT,
        http.get(url).bearer_auth(&record.discovery.token).send(),
    )
    .await;
    let response = match response {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => return Verdict::Unreachable("not reachable".into()),
        Err(_) => return Verdict::Unreachable("did not answer in time".into()),
    };
    let status = response.status();
    if status.is_server_error() {
        return Verdict::Unreachable(format!("answered {status}"));
    }
    if !status.is_success() {
        return Verdict::Rejected(format!("answered {status}"));
    }
    let info = match tokio::time::timeout(INFO_TIMEOUT, response.json::<Info>()).await {
        Ok(Ok(info)) => info,
        Ok(Err(_)) => return Verdict::Rejected("not a Haarness server".into()),
        Err(_) => return Verdict::Unreachable("did not answer in time".into()),
    };
    judge(record, info)
}

fn judge(record: &Record, info: Info) -> Verdict {
    if info.name != "haarness" {
        return Verdict::Rejected("not a Haarness server".into());
    }
    if info.pid.is_some_and(|pid| pid != record.discovery.pid) {
        return Verdict::Rejected("another process answers on this port".into());
    }
    // Both come from the server's own canonical path. Comparing them as text
    // keeps the bridge out of project folders: Chrome would be asked for
    // access to Documents or Desktop.
    let workspace = match (info.workspace, record.discovery.workspace.as_deref()) {
        (Some(actual), Some(recorded)) if Path::new(&actual) != Path::new(recorded) => {
            return Verdict::Rejected("serves another folder".into());
        }
        (Some(actual), _) => actual,
        (None, Some(recorded)) => recorded.to_owned(),
        (None, None) => return Verdict::Rejected("names no project folder".into()),
    };
    Verdict::Live(Server {
        capable: info
            .capabilities
            .iter()
            .any(|capability| capability == super::EXTENSION_CAPABILITY),
        id: info.server_instance_id,
        workspace,
        version: info.version,
        base: record.base.clone(),
        token: record.discovery.token.clone(),
        record: std::sync::Arc::new(record.clone()),
    })
}

enum State {
    Live(Server),
    Rejected,
    Waiting { backoff: Backoff, retry_at: Instant },
}

struct Entry {
    /// What the record said when it was checked; a new server writes a new one.
    key: (String, String, u32),
    state: State,
}

/// The servers found so far, keyed by record file. A record is asked again
/// only when it changes, when its connection ended (`forget`), or, after a
/// transient failure, once its backoff has passed.
pub(crate) struct Discovery {
    caches: Vec<PathBuf>,
    http: reqwest::Client,
    alive: fn(u32) -> bool,
    pacing: Pacing,
    entries: HashMap<PathBuf, Entry>,
}

/// Retry a server that did not answer after 250 ms, doubling to 5 s.
pub(crate) fn retry_pacing() -> Pacing {
    Pacing {
        retry: Duration::from_millis(250),
        retry_max: Duration::from_secs(5),
        stable: Duration::from_secs(30),
        drops: u32::MAX,
        cooldown: Duration::from_secs(5),
        cooldown_max: Duration::from_secs(5),
    }
}

impl Discovery {
    pub(crate) fn new(caches: Vec<PathBuf>, http: reqwest::Client) -> Self {
        Self {
            caches,
            http,
            alive: pid_alive,
            pacing: retry_pacing(),
            entries: HashMap::new(),
        }
    }

    /// How to tell a record's process is gone (tests pretend).
    pub(crate) fn with_liveness(mut self, alive: fn(u32) -> bool) -> Self {
        self.alive = alive;
        self
    }

    /// Every live server, deduplicated by instance.
    pub(crate) async fn scan(&mut self) -> Vec<Server> {
        let records = records(&self.caches, self.alive);
        let present: HashSet<&PathBuf> = records.iter().map(|record| &record.file).collect();
        self.entries.retain(|file, _| present.contains(file));
        let now = Instant::now();
        let http = &self.http;
        let mut checks = Vec::new();
        for record in &records {
            let key = (
                record.discovery.url.clone(),
                record.discovery.token.clone(),
                record.discovery.pid,
            );
            match self.entries.get(&record.file) {
                Some(entry) if entry.key == key => match &entry.state {
                    State::Live(_) | State::Rejected => continue,
                    State::Waiting { retry_at, .. } if *retry_at > now => continue,
                    State::Waiting { .. } => {}
                },
                _ => {}
            }
            checks.push(async move { (record, key, verify(http, record).await) });
        }
        for (record, key, verdict) in futures::future::join_all(checks).await {
            let state = match verdict {
                Verdict::Live(server) => State::Live(server),
                Verdict::Rejected(reason) => {
                    tracing::info!(record = %record.file.display(), %reason, "ignoring a Haarness server record");
                    State::Rejected
                }
                Verdict::Unreachable(reason) => {
                    let mut backoff = match self.entries.remove(&record.file) {
                        Some(Entry {
                            key: previous,
                            state: State::Waiting { backoff, .. },
                        }) if previous == key => backoff,
                        _ => Backoff::new(self.pacing),
                    };
                    tracing::debug!(record = %record.file.display(), %reason, "Haarness server not reachable yet");
                    let retry_at = Instant::now() + backoff.wait(Link::Failed);
                    State::Waiting { backoff, retry_at }
                }
            };
            self.entries
                .insert(record.file.clone(), Entry { key, state });
        }
        let mut seen = HashSet::new();
        records
            .iter()
            .filter_map(|record| match &self.entries.get(&record.file)?.state {
                State::Live(server) => Some(server.clone()),
                _ => None,
            })
            .filter(|server| seen.insert(server.id.clone()))
            .collect()
    }

    /// Ask this server again at the next scan: its connection ended, and the
    /// record may now belong to a replacement.
    pub(crate) fn forget(&mut self, id: &str) {
        self.entries
            .retain(|_, entry| !matches!(&entry.state, State::Live(server) if server.id == id));
    }

    /// When a server that did not answer is next worth asking.
    pub(crate) fn next_retry(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|entry| match entry.state {
                State::Waiting { retry_at, .. } => Some(retry_at),
                _ => None,
            })
            .min()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use serde_json::{Value, json};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    pub(crate) fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("haarness-{name}-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        canonical(&root)
    }

    pub(crate) fn write_record(
        cache: &Path,
        folder: &str,
        url: &str,
        token: &str,
        pid: u32,
        workspace: &str,
    ) -> PathBuf {
        let dir = cache.join("workspaces").join(folder);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("runtime.json");
        fs::write(
            &file,
            serde_json::to_vec(
                &json!({"url": url, "token": token, "pid": pid, "workspace": workspace}),
            )
            .unwrap(),
        )
        .unwrap();
        file
    }

    /// A fake `/v1/info` that answers `info` and counts requests; `token`
    /// must be presented.
    pub(crate) async fn fake_info(info: Value, token: &'static str) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = Router::new().route(
            "/v1/info",
            get(move |headers: axum::http::HeaderMap| {
                let info = info.clone();
                counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    if headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        != Some(&format!("Bearer {token}"))
                    {
                        return Err(axum::http::StatusCode::UNAUTHORIZED);
                    }
                    Ok(Json(info))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, hits)
    }

    pub(crate) fn info(id: &str, workspace: &str, pid: Option<u32>, capable: bool) -> Value {
        let mut capabilities = vec!["browser.v1"];
        if capable {
            capabilities.push(super::super::EXTENSION_CAPABILITY);
        }
        let mut info = json!({"name":"haarness","version":"0.1.1","schemaVersion":"1",
            "serverInstanceId": id, "capabilities": capabilities, "workspace": workspace});
        if let Some(pid) = pid {
            info["pid"] = json!(pid);
        }
        info
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: HashMap<String, OsString> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect();
        move |key| pairs.get(key).cloned()
    }

    #[test]
    fn roots_are_the_environments_the_default_and_the_extra_ones_once_each() {
        // Absolute on every platform, and never created.
        let base = std::env::temp_dir().join(format!("haarness-roots-{}", uuid::Uuid::now_v7()));
        let home = base.join("home");
        let text = |path: PathBuf| path.display().to_string();
        let only_default = data_roots(&env_of(&[]), Some(&home));
        assert_eq!(only_default.len(), 1);
        assert_eq!(only_default[0].cache, home.join(".cache").join("haarness"));
        assert_eq!(
            only_default[0].data,
            home.join(".local/share").join("haarness")
        );

        let extra = std::env::join_paths([
            base.join("extra"),
            PathBuf::from("relative"),
            base.join("extra"),
        ])
        .unwrap();
        let roots = data_roots(
            &env_of(&[
                ("HAARNESS_DATA_DIR", &text(base.join("isolated"))),
                ("XDG_CACHE_HOME", &text(base.join("elsewhere"))),
                ("HAARNESS_BROWSER_EXTRA_ROOTS", extra.to_str().unwrap()),
            ]),
            Some(&home),
        );
        let caches: Vec<_> = roots.iter().map(|root| root.cache.clone()).collect();
        assert_eq!(
            caches,
            [
                base.join("isolated").join("cache"),
                home.join(".cache").join("haarness"),
                base.join("extra").join("cache"),
            ]
        );
        assert_eq!(roots[0].data, base.join("isolated").join("data"));

        // XDG variables move the bridge's own root; the default stays as Dock-launched apps see it.
        let roots = data_roots(
            &env_of(&[("XDG_CACHE_HOME", &text(base.join("xdg")))]),
            Some(&home),
        );
        assert_eq!(roots[0].cache, base.join("xdg").join("haarness"));
        assert_eq!(roots[1].cache, home.join(".cache").join("haarness"));
        // HAARNESS_HOME wins over XDG; a relative one is ignored like Paths::new refuses it.
        let roots = data_roots(
            &env_of(&[
                ("HAARNESS_HOME", &text(base.join("hh"))),
                ("XDG_CACHE_HOME", &text(base.join("x"))),
            ]),
            Some(&home),
        );
        assert_eq!(roots[0].cache, base.join("hh").join("cache"));
        let roots = data_roots(&env_of(&[("HAARNESS_HOME", "hh")]), Some(&home));
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn verbatim_windows_paths_become_plain_drive_paths() {
        assert_eq!(
            plain(PathBuf::from(r"\\?\C:\Users\a")),
            PathBuf::from(r"C:\Users\a")
        );
        assert_eq!(
            plain(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\?\UNC\server\share")
        );
        assert_eq!(plain(PathBuf::from("/home/a")), PathBuf::from("/home/a"));
    }

    #[cfg(unix)]
    #[test]
    fn roots_are_compared_by_their_canonical_paths() {
        let real = temp("root-real");
        let link = real
            .parent()
            .unwrap()
            .join(format!("haarness-root-link-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(real.join("cache")).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let roots = data_roots(
            &env_of(&[
                ("HAARNESS_DATA_DIR", &real.display().to_string()),
                ("HAARNESS_BROWSER_EXTRA_ROOTS", &link.display().to_string()),
            ]),
            None,
        );
        assert_eq!(roots.len(), 1, "{roots:?}");
        fs::remove_file(&link).unwrap();
        fs::remove_dir_all(real).unwrap();
    }

    #[test]
    fn only_loopback_http_urls_are_this_computers() {
        for (url, base) in [
            ("http://127.0.0.1:4096", Some("http://127.0.0.1:4096/")),
            ("http://0.0.0.0:4096", Some("http://127.0.0.1:4096/")),
            ("http://[::]:4096/", Some("http://[::1]:4096/")),
            ("http://[::1]:4096", Some("http://[::1]:4096/")),
            ("http://localhost:4096/x?y", Some("http://127.0.0.1:4096/")),
            ("http://127.0.0.2:4096", Some("http://127.0.0.2:4096/")),
            ("http://192.168.1.2:4096", None),
            ("http://example.com:4096", None),
            ("https://127.0.0.1:4096", None),
            ("http://127.0.0.1", None),
            ("not a url", None),
        ] {
            assert_eq!(
                loopback_base(url).map(|url| url.to_string()).as_deref(),
                base,
                "{url}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_finished_process_is_not_alive() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_alive(pid));
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX));
    }

    #[tokio::test]
    async fn live_servers_are_verified_deduplicated_and_kept_until_their_record_changes() {
        let root = temp("discovery");
        let cache = root.join("cache");
        let other = root.join("other-cache");
        let me = std::process::id();
        let (capable, capable_hits) =
            fake_info(info("server-a", "/work/a", Some(me), true), "token-a").await;
        let (old, _) = fake_info(info("server-b", "/work/b", None, false), "token-b").await;
        let (impostor, _) =
            fake_info(info("server-c", "/work/c", Some(me + 1), true), "token-c").await;
        let (moved, _) = fake_info(
            info("server-d", "/work/elsewhere", Some(me), true),
            "token-d",
        )
        .await;
        write_record(&cache, "a", &capable, "token-a", me, "/work/a");
        // The same server found through a second root is listed once.
        write_record(&other, "a", &capable, "token-a", me, "/work/a");
        write_record(&cache, "b", &old, "token-b", me, "/work/b");
        write_record(&cache, "c", &impostor, "token-c", me, "/work/c");
        write_record(&cache, "d", &moved, "token-d", me, "/work/d");
        // A record whose process is gone is never asked.
        write_record(&cache, "dead", &capable, "token-a", 999_999, "/work/a");
        // Not this computer.
        write_record(&cache, "remote", "http://10.0.0.1:80", "x", me, "/work/x");
        fs::create_dir_all(cache.join("workspaces/empty")).unwrap();
        fs::create_dir_all(cache.join("workspaces/garbage")).unwrap();
        fs::write(cache.join("workspaces/garbage/runtime.json"), "{").unwrap();

        let mut discovery = Discovery::new(vec![cache.clone(), other.clone()], http_client())
            .with_liveness(|pid| pid == std::process::id());
        let servers = discovery.scan().await;
        let listed: Vec<_> = servers
            .iter()
            .map(|server| (server.id.as_str(), server.capable))
            .collect();
        assert_eq!(listed, [("server-a", true), ("server-b", false)]);
        assert_eq!(servers[0].workspace, "/work/a");
        assert_eq!(servers[0].name(), "a");
        assert_eq!(servers[0].base.as_str(), format!("{capable}/"));
        assert_eq!(
            capable_hits.load(Ordering::SeqCst),
            2,
            "each record is asked once"
        );

        // Unchanged records are not asked again.
        assert_eq!(discovery.scan().await.len(), 2);
        assert_eq!(capable_hits.load(Ordering::SeqCst), 2);
        // After its connection ended, a server is asked again.
        discovery.forget("server-a");
        assert_eq!(discovery.scan().await.len(), 2);
        assert_eq!(capable_hits.load(Ordering::SeqCst), 4);
        // A record that disappears takes its server with it.
        fs::remove_file(cache.join("workspaces/b/runtime.json")).unwrap();
        let servers = discovery.scan().await;
        assert_eq!(servers.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_server_is_retried_with_backoff() {
        let root = temp("discovery-retry");
        let cache = root.join("cache");
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);
        write_record(
            &cache,
            "a",
            &format!("http://{address}"),
            "token-a",
            std::process::id(),
            "/work/a",
        );
        let mut discovery = Discovery::new(vec![cache.clone()], http_client());
        assert!(discovery.scan().await.is_empty());
        let first = discovery.next_retry().expect("a retry is planned");
        assert!(first <= Instant::now() + Duration::from_millis(250));
        // Until the retry is due nothing is asked; afterwards the wait grows.
        assert!(discovery.scan().await.is_empty());
        assert_eq!(discovery.next_retry(), Some(first));
        tokio::time::sleep_until(first).await;
        assert!(discovery.scan().await.is_empty());
        let second = discovery.next_retry().unwrap();
        assert!(
            second >= first + Duration::from_millis(350),
            "{:?}",
            second - first
        );

        // Once the server answers on that port it is found.
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let app = Router::new().route(
            "/v1/info",
            get(|| async { Json(info("server-a", "/work/a", Some(std::process::id()), true)) }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep_until(second).await;
        let servers = discovery.scan().await;
        assert_eq!(servers.len(), 1);
        assert!(discovery.next_retry().is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
