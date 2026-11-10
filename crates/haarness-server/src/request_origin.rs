//! Who sent a request: a person, or a program Haarness started on this computer for an agent (its command, a hook).
//! Only a person lets agents do more without asking, so the server asks this for the few requests that would: raising
//! a chat's approvals, a turn that approves its own requests, an answer to an agent's request or question, a terminal
//! (what runs there counts as a person's), setting up or starting a schedule, more reach for computer use. Requests
//! from another computer, those the connector brings from phones and Haarness Cloud, and what a person runs in a
//! terminal Haarness hosts come from people; computer use also refuses those terminals.
//!
//! The program at the other end of a connection is found on the first guarded request on it ([`Client`]), and every
//! later one is judged by that. A request another Haarness program forwards (the host gateway, a front) carries the
//! forwarder's judgement of its own sender in [`SENDER_HEADER`], vouched for by this server's key in
//! [`FORWARDER_HEADER`]; the key is only ever in memory, here and in the forwarders a person runs, which ask for it
//! ([`forwarding_key_of`]). A sender's word without the key counts for nothing but an unknown sender.
use axum::http::{HeaderMap, HeaderValue};
use haarness_core::{
    AgentRuntime,
    process_tree::{Peer, peer},
};
use haarness_protocol::{ApprovalMode, ApprovalModeSetting, ThreadId};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

/// The header a forwarding Haarness program sets on what it forwards: [`Sender`]'s word for whoever sent it the
/// request, in place of whatever the sender wrote.
pub const SENDER_HEADER: &str = "x-haarness-sender";

/// The header that vouches for [`SENDER_HEADER`]: the key of the server the request is forwarded to.
pub const FORWARDER_HEADER: &str = "x-haarness-forwarder";

/// The header on an answer refusing a request whose sender could not be found, so a forwarder asks for the key again.
pub const UNKNOWN_SENDER_HEADER: &str = "x-haarness-unknown-sender";

/// This server's key, made when it starts and never written anywhere.
static KEY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    use rand::RngCore as _;
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
});

/// This server's key, for a forwarder a person runs ([`forwarding_key_of`]).
#[must_use]
pub fn forwarding_key() -> &'static str {
    &KEY
}

/// The addresses this server listens on (see [`note_own_listener`]).
static OWN_LISTENERS: std::sync::LazyLock<parking_lot::Mutex<Vec<SocketAddr>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that this server listens on `address`, where what it forwards to itself is vouched for with its own key.
pub fn note_own_listener(address: SocketAddr) {
    OWN_LISTENERS.lock().push(address);
}

/// Whether `base` is one of this server's own listeners ([`note_own_listener`]): its address, or a loopback one on the
/// port of a listener on every address.
fn own_listener(base: &str) -> bool {
    let Ok(url) = url::Url::parse(base) else {
        return false;
    };
    let ip = match url.host() {
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip),
        _ => return false,
    };
    let port = url.port_or_known_default();
    OWN_LISTENERS.lock().iter().any(|own| {
        Some(own.port()) == port
            && (own.ip() == ip || (own.ip().is_unspecified() && ip.is_loopback()))
    })
}

/// The keys of the servers this process forwards to, by address and token: none for one that would not say.
type Keys = HashMap<(String, String), Option<String>>;

/// [`Keys`] as this process has asked for them.
static KEYS: std::sync::LazyLock<parking_lot::Mutex<Keys>> =
    std::sync::LazyLock::new(Default::default);

/// The key of the server at `base` (signed in with `token`), to vouch for what this process forwards there: this
/// server's own for its own listener, and no other server's ever. Another server is asked once, and has none here when
/// it will not say (an older server, or one that did not take this process for a person's); what is forwarded to it
/// then counts as from an unknown sender.
pub async fn forwarding_key_of(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Option<String> {
    if own_listener(base) {
        return Some(forwarding_key().to_owned());
    }
    let index = (base.trim_end_matches('/').to_owned(), token.to_owned());
    if let Some(key) = KEYS.lock().get(&index) {
        return key.clone();
    }
    let asked = client
        .get(format!("{}/v1/host/forwarding", index.0))
        .bearer_auth(token)
        .send()
        .await
        .ok()
        .filter(|response| response.status().is_success());
    let key = match asked {
        Some(response) => response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| value["key"].as_str().map(str::to_owned)),
        None => None,
    };
    KEYS.lock().insert(index, key.clone());
    key
}

/// Forgets the key of the server at `base`, which refused a forwarded request as from an unknown sender: it may have
/// started again with another, or now take this process for a person's.
pub fn forget_forwarding_key(base: &str, token: &str) {
    KEYS.lock()
        .remove(&(base.trim_end_matches('/').to_owned(), token.to_owned()));
}

/// Whether the answer to a forwarded request refused it as from an unknown sender, before doing anything: the
/// forwarder asks for the server's key again ([`forget_forwarding_key`]) and sends the request once more.
#[must_use]
pub fn unknown_sender(headers: &HeaderMap) -> bool {
    headers.contains_key(UNKNOWN_SENDER_HEADER)
}

/// The headers a forwarder sets on a request it forwards, in place of any the sender set: that `sender` sent it,
/// vouched for with `key`, the key of the server it goes to, when the forwarder has it.
#[must_use]
pub fn forwarded_headers(sender: Sender, key: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(SENDER_HEADER, sender.header());
    if let Some(key) = key.and_then(|key| HeaderValue::from_str(key).ok()) {
        headers.insert(FORWARDER_HEADER, key);
    }
    headers
}

/// How long a guarded request waits for its connection's program to be found before it counts as unknown.
const ATTRIBUTION_WAIT: Duration = Duration::from_secs(10);

/// Who sent a request, from least to most guarded against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sender {
    /// A person: a client outside Haarness, another computer, or a computer where this cannot be told.
    Person,
    /// A program a person runs in a terminal Haarness opened, such as Desktop's terminal panel.
    Terminal,
    /// A program on this computer that could not be found.
    Unknown,
    /// A program Haarness started for an agent: its command and what that runs or leaves running, a hook.
    Agent,
}

impl Sender {
    fn word(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Terminal => "terminal",
            Self::Unknown => "unknown",
            Self::Agent => "agent",
        }
    }

    fn from_word(word: &[u8]) -> Self {
        match word {
            b"person" => Self::Person,
            b"terminal" => Self::Terminal,
            b"agent" => Self::Agent,
            _ => Self::Unknown,
        }
    }

    /// The header value a forwarder sets for this sender.
    #[must_use]
    pub fn header(self) -> HeaderValue {
        HeaderValue::from_static(self.word())
    }

    /// Whether Haarness started the sender, a person's terminal included.
    #[must_use]
    pub fn inside(self) -> bool {
        self >= Self::Terminal
    }
}

/// The other end of a connection this server accepted. Its program is looked for on the first guarded request on it,
/// while that request waits, so a sender that leaves before the answer is not found; once found, it stands for the
/// connection's life. A look that found nothing is not kept: the next guarded request looks again.
#[derive(Clone, Debug)]
pub struct Client {
    pub address: SocketAddr,
    peer: Arc<tokio::sync::Mutex<Option<Peer>>>,
    lookup: fn(SocketAddr) -> Peer,
}

impl Client {
    /// The other end of a connection from `address`.
    #[must_use]
    pub fn accepted(address: SocketAddr) -> Self {
        Self::looked_up_with(address, peer)
    }

    /// The other end of a connection whose program is already known (tests).
    #[must_use]
    pub fn known(address: SocketAddr, peer: Peer) -> Self {
        Self {
            address,
            peer: Arc::new(tokio::sync::Mutex::new(Some(peer))),
            lookup: |_| Peer::Unknown,
        }
    }

    /// The other end of a connection whose program `lookup` finds (tests).
    #[must_use]
    pub fn looked_up_with(address: SocketAddr, lookup: fn(SocketAddr) -> Peer) -> Self {
        Self {
            address,
            peer: Arc::default(),
            lookup,
        }
    }

    async fn peer(&self) -> Peer {
        let mut known = self.peer.lock().await;
        if let Some(peer) = *known {
            return peer;
        }
        let (address, lookup) = (self.address, self.lookup);
        let found = tokio::time::timeout(
            ATTRIBUTION_WAIT,
            tokio::task::spawn_blocking(move || lookup(address)),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(Peer::Unknown);
        if found != Peer::Unknown {
            *known = Some(found);
        }
        found
    }
}

/// A server's listener, with what it does to each connection it accepts (`tap`, as keep-alive settings) before the
/// connection's program is looked for.
pub struct Accepting<F> {
    pub listener: tokio::net::TcpListener,
    pub tap: F,
}

impl<F> axum::serve::Listener for Accepting<F>
where
    F: FnMut(&mut tokio::net::TcpStream) + Send + 'static,
{
    type Io = tokio::net::TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (mut io, address) = axum::serve::Listener::accept(&mut self.listener).await;
        (self.tap)(&mut io);
        (io, address)
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

impl<F> axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, Accepting<F>>>
    for Client
where
    F: FnMut(&mut tokio::net::TcpStream) + Send + 'static,
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, Accepting<F>>) -> Self {
        Self::accepted(*stream.remote_addr())
    }
}

impl
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>>
    for Client
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Self::accepted(*stream.remote_addr())
    }
}

/// Whether what this process sends its own server, with no forwarder's word, is a person's (see
/// [`hear_this_process_as_a_person`]).
static OWN_REQUESTS_ARE_A_PERSONS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// For a process that hosts a server and is its own client, as tests do: what it sends that server without a
/// forwarder's word counts as a person's. A server never calls this; without it, such a request is unknown.
pub fn hear_this_process_as_a_person() {
    OWN_REQUESTS_ARE_A_PERSONS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The connection a request came on, when it came over the network.
pub type Connection = Option<axum::Extension<axum::extract::ConnectInfo<Client>>>;

/// Who sent a request that came on `connection` with `headers`. A forwarded request is judged by the word its
/// forwarder vouched for with this server's key, and never more leniently than the forwarder itself; a word without
/// the key makes the sender unknown. A request with no connection was made inside this server.
pub async fn sender(connection: &Connection, headers: &HeaderMap) -> Sender {
    let forwarded = match (headers.get(SENDER_HEADER), headers.get(FORWARDER_HEADER)) {
        (None, _) => None,
        (Some(word), Some(key)) if same(key.as_bytes(), forwarding_key().as_bytes()) => {
            Some(Sender::from_word(word.as_bytes()))
        }
        (Some(_), _) => return Sender::Unknown,
    };
    let Some(axum::Extension(axum::extract::ConnectInfo(client))) = connection else {
        return forwarded.unwrap_or(Sender::Person);
    };
    let peer = client.peer().await;
    let own = match peer {
        Peer::Unknown => Sender::Unknown,
        // This server, or another, sending for someone it heard from: only its vouched word tells who.
        Peer::Itself | Peer::Server => {
            if let Some(forwarded) = forwarded {
                return forwarded;
            }
            let own = peer == Peer::Itself
                && OWN_REQUESTS_ARE_A_PERSONS.load(std::sync::atomic::Ordering::Relaxed);
            if own { Sender::Person } else { Sender::Unknown }
        }
        Peer::Program { agent: true, .. } => Sender::Agent,
        Peer::Program { inside: true, .. } => Sender::Terminal,
        // Another computer, one where the program cannot be told, or a program outside Haarness.
        Peer::Elsewhere | Peer::Untold | Peer::Program { .. } => Sender::Person,
    };
    forwarded.map_or(own, |forwarded| forwarded.max(own))
}

/// Who asks for this server's key ([`forwarding_key`]), on `connection` with `headers`: as [`sender`], except that
/// another Haarness server of the person's, asking for itself (with no forwarded word), may have it to vouch with.
pub async fn key_asker(connection: &Connection, headers: &HeaderMap) -> Sender {
    if headers.get(SENDER_HEADER).is_none()
        && let Some(axum::Extension(axum::extract::ConnectInfo(client))) = connection
        && client.peer().await == Peer::Server
    {
        return Sender::Person;
    }
    sender(connection, headers).await
}

/// Whether two keys are the same, taking as long whichever byte differs.
fn same(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |differ, (left, right)| differ | (left ^ right))
            == 0
}

/// How much `mode` lets agents do without asking.
const fn reach(mode: ApprovalMode) -> u8 {
    match mode {
        ApprovalMode::Review => 0,
        ApprovalMode::Auto => 1,
        ApprovalMode::Yolo => 2,
    }
}

/// Whether `setting` would let the agents of chat `id` do more without asking than they may now. `Inherit` takes the
/// mode of the agent that delegated to the chat, or the server's default for a chat of its own.
pub(crate) async fn raises(
    runtime: &AgentRuntime,
    id: ThreadId,
    setting: ApprovalModeSetting,
) -> haarness_core::CoreResult<bool> {
    let wanted = match setting {
        ApprovalModeSetting::Review => return Ok(false),
        ApprovalModeSetting::Auto => ApprovalMode::Auto,
        ApprovalModeSetting::Yolo => ApprovalMode::Yolo,
        ApprovalModeSetting::Inherit => {
            let thread = runtime.get_thread(id).await?;
            match thread.parent_id.filter(|_| thread.is_subagent) {
                Some(parent) => match runtime.approval_mode(parent).await {
                    Ok(info) => info.mode,
                    Err(haarness_core::CoreError::NotFound(_)) => runtime.approval_mode_default(),
                    Err(error) => return Err(error),
                },
                None => runtime.approval_mode_default(),
            }
        }
    };
    Ok(reach(wanted) > reach(runtime.approval_mode(id).await?.mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    use haarness_core::{EventBus, StaticProviderRegistry};
    use haarness_protocol::{CreateThreadRequest, SetApprovalModeRequest};
    use std::sync::Arc;

    #[tokio::test]
    async fn only_more_reach_than_a_chat_has_now_counts_as_raising_it() {
        use ApprovalModeSetting::{Auto, Inherit, Review, Yolo};
        let runtime = AgentRuntime::new(
            Arc::new(haarness_store::SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(Vec::new())),
            Arc::new(crate::NativeToolRegistry::new(std::env::temp_dir())),
            EventBus::new(16),
        );
        let create = |parent| {
            runtime.create_thread(CreateThreadRequest {
                parent_id: parent,
                provider: Some("echo".into()),
                model: Some("echo-1".into()),
                ..Default::default()
            })
        };
        let set = |id, mode| {
            runtime.set_approval_mode(
                id,
                SetApprovalModeRequest {
                    mode,
                    revision: None,
                },
            )
        };
        let raised = async |id, settings: &[ApprovalModeSetting]| {
            let mut answers = Vec::new();
            for setting in settings {
                answers.push(raises(&runtime, id, *setting).await.unwrap());
            }
            answers
        };
        let all = [Review, Auto, Yolo, Inherit];
        // A chat of its own follows the server's default, Auto.
        let chat = create(None).await.unwrap().id;
        assert_eq!(raised(chat, &all).await, [false, false, true, false]);
        // In Review, Auto and Yolo raise it, and so does following the default again.
        set(chat, Review).await.unwrap();
        assert_eq!(raised(chat, &all).await, [false, true, true, true]);
        // In Yolo nothing does.
        set(chat, Yolo).await.unwrap();
        assert_eq!(raised(chat, &all).await, [false, false, false, false]);
        // An agent's chat inherits from the chat that started it.
        let agent = create(Some(chat)).await.unwrap().id;
        set(agent, Review).await.unwrap();
        assert_eq!(raised(agent, &all).await, [false, true, true, true]);
        set(chat, Auto).await.unwrap();
        assert_eq!(raised(agent, &all).await, [false, true, true, true]);
        set(agent, Auto).await.unwrap();
        assert_eq!(raised(agent, &all).await, [false, false, true, false]);
    }

    #[tokio::test]
    async fn a_forwarders_word_counts_only_with_this_servers_key() {
        let address: SocketAddr = "127.0.0.1:50123".parse().unwrap();
        let judge = |peer: Peer, word: Option<&'static str>, key: Option<&str>| {
            let key = key.map(str::to_owned);
            async move {
                let mut headers = HeaderMap::new();
                if let Some(word) = word {
                    headers.insert(SENDER_HEADER, HeaderValue::from_static(word));
                }
                if let Some(key) = key {
                    headers.insert(FORWARDER_HEADER, HeaderValue::from_str(&key).unwrap());
                }
                let connection = Some(axum::Extension(axum::extract::ConnectInfo(Client::known(
                    address, peer,
                ))));
                sender(&connection, &headers).await
            }
        };
        let program = |inside, agent| Peer::Program { inside, agent };
        let key = Some(forwarding_key());
        // This server, or another, forwarding: its vouched word, unknown without one.
        assert_eq!(
            judge(Peer::Itself, Some("person"), key).await,
            Sender::Person
        );
        assert_eq!(judge(Peer::Server, Some("agent"), key).await, Sender::Agent);
        assert_eq!(judge(Peer::Itself, None, None).await, Sender::Unknown);
        assert_eq!(judge(Peer::Server, None, None).await, Sender::Unknown);
        // A word without the key, or with another, says nothing but that the sender is unknown.
        assert_eq!(
            judge(Peer::Itself, Some("person"), None).await,
            Sender::Unknown
        );
        assert_eq!(
            judge(program(false, false), Some("person"), Some("guess")).await,
            Sender::Unknown
        );
        // A vouched word never makes a sender laxer than its own program.
        assert_eq!(
            judge(program(true, true), Some("person"), key).await,
            Sender::Agent
        );
        assert_eq!(
            judge(program(false, false), None, None).await,
            Sender::Person
        );
        assert_eq!(
            judge(program(true, false), None, None).await,
            Sender::Terminal
        );
        assert_eq!(judge(Peer::Elsewhere, None, None).await, Sender::Person);
        assert_eq!(judge(Peer::Untold, None, None).await, Sender::Person);
        // Made inside this server, with no connection.
        assert_eq!(sender(&None, &HeaderMap::new()).await, Sender::Person);
        assert!(Sender::Terminal.inside() && !Sender::Person.inside());
    }

    static LOOKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// The first look finds nothing (a `ps` that failed under load, say); every later one finds a person.
    fn flaky_lookup(_address: SocketAddr) -> Peer {
        if LOOKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Peer::Unknown
        } else {
            Peer::Program {
                inside: false,
                agent: false,
            }
        }
    }

    #[tokio::test]
    async fn a_connections_program_is_looked_for_when_first_needed_and_kept_once_found() {
        let client = Client::looked_up_with("127.0.0.1:50124".parse().unwrap(), flaky_lookup);
        // Nothing is looked for until a guarded request needs it.
        assert_eq!(LOOKS.load(std::sync::atomic::Ordering::SeqCst), 0);
        let connection = Some(axum::Extension(axum::extract::ConnectInfo(client)));
        let headers = HeaderMap::new();
        // A look that found nothing is not kept: the next request looks again, and keeps what it finds.
        assert_eq!(sender(&connection, &headers).await, Sender::Unknown);
        assert_eq!(sender(&connection, &headers).await, Sender::Person);
        assert_eq!(sender(&connection, &headers).await, Sender::Person);
        assert_eq!(LOOKS.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_forwarder_vouches_with_its_own_key_only_to_its_own_listener() {
        // A listener on every address, and one on loopback.
        note_own_listener("0.0.0.0:50301".parse().unwrap());
        note_own_listener("127.0.0.1:50302".parse().unwrap());
        assert!(own_listener("http://127.0.0.1:50301"));
        assert!(own_listener("http://127.0.0.1:50302/"));
        assert!(!own_listener("http://[::1]:50302"));
        assert!(!own_listener("http://192.0.2.1:50302"));
        assert!(!own_listener("http://localhost:50302"));
        let client = reqwest::Client::new();
        assert_eq!(
            forwarding_key_of(&client, "http://127.0.0.1:50302", "t")
                .await
                .as_deref(),
            Some(forwarding_key())
        );
        // A listener that will not say its key, and one that is gone, get no key at all.
        let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_address = other.local_addr().unwrap();
        let refusing = tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async { axum::http::StatusCode::FORBIDDEN });
            axum::serve(other, app).await.unwrap();
        });
        let refused = forwarding_key_of(&client, &format!("http://{other_address}"), "t").await;
        refusing.abort();
        let gone = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let unreachable = forwarding_key_of(&client, &format!("http://{gone}"), "t").await;
        assert_eq!((refused, unreachable), (None, None));
        // The headers carry the sender's word, and the key only when there is one.
        let keyed = forwarded_headers(Sender::Agent, Some("k"));
        assert_eq!(keyed.get(SENDER_HEADER).unwrap(), "agent");
        assert_eq!(keyed.get(FORWARDER_HEADER).unwrap(), "k");
        let bare = forwarded_headers(Sender::Person, None);
        assert_eq!(bare.get(SENDER_HEADER).unwrap(), "person");
        assert!(bare.get(FORWARDER_HEADER).is_none());
    }

    #[test]
    fn modes_rank_by_what_agents_may_do_unasked() {
        assert!(reach(ApprovalMode::Review) < reach(ApprovalMode::Auto));
        assert!(reach(ApprovalMode::Auto) < reach(ApprovalMode::Yolo));
    }
}
