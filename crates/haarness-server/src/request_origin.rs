//! Who sent a request: a person, or a program Haarness started on this computer for an agent (its command, a hook).
//! Only a person lets agents do more without asking, so the server asks this for the few requests that would: raising
//! a chat's approvals, a turn that approves its own requests, an answer to an agent's request or question, a terminal
//! (what runs there counts as a person's), setting up or starting a schedule, more reach for computer use. Requests
//! from another computer, those the connector brings from phones and Haarness Cloud, and what a person runs in a
//! terminal Haarness hosts come from people; computer use also refuses those terminals.
//!
//! The program at the other end of a connection is found once, in the background, when the connection is accepted
//! ([`Client`]), and every request on it is judged by that. A request another Haarness program forwards (the host
//! gateway, a front) carries the forwarder's judgement of its own sender in [`SENDER_HEADER`].
use axum::http::{HeaderMap, HeaderValue};
use haarness_core::{
    AgentRuntime,
    process_tree::{Peer, peer},
};
use haarness_protocol::{ApprovalMode, ApprovalModeSetting, ThreadId};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::OnceCell;

/// The header a forwarding Haarness program sets on what it forwards: [`Sender`]'s word for whoever sent it the
/// request. It replaces any the sender wrote; a server takes it at its word only from itself, and otherwise as more
/// that the request might be.
pub const SENDER_HEADER: &str = "x-haarness-sender";

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

/// The other end of a connection this server accepted. Its program is found once, in the background, from the moment
/// the connection is accepted, so a request on it does not wait for that and its sender cannot leave before it.
#[derive(Clone, Debug)]
pub struct Client {
    pub address: SocketAddr,
    peer: Arc<OnceCell<Peer>>,
}

impl Client {
    /// The other end of a connection from `address`, its program looked for at once.
    #[must_use]
    pub fn accepted(address: SocketAddr) -> Self {
        let client = Self {
            address,
            peer: Arc::default(),
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let cell = client.peer.clone();
            runtime.spawn(async move {
                cell.get_or_init(|| attribute(address)).await;
            });
        }
        client
    }

    /// The other end of a connection whose program is already known (tests).
    #[must_use]
    pub fn known(address: SocketAddr, peer: Peer) -> Self {
        Self {
            address,
            peer: Arc::new(OnceCell::new_with(Some(peer))),
        }
    }

    async fn peer(&self) -> Peer {
        let address = self.address;
        tokio::time::timeout(
            ATTRIBUTION_WAIT,
            self.peer.get_or_init(|| attribute(address)),
        )
        .await
        .map_or(Peer::Unknown, |peer| *peer)
    }
}

async fn attribute(address: SocketAddr) -> Peer {
    tokio::task::spawn_blocking(move || peer(address))
        .await
        .unwrap_or(Peer::Unknown)
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

/// Who sent a request that came on `connection` with `headers`. A request this server forwarded to itself is judged
/// by the sender its forwarder found, and is unknown without one; any other may only be judged stricter by the header.
/// A request with no connection was made inside this server.
pub async fn sender(connection: &Connection, headers: &HeaderMap) -> Sender {
    let forwarded = headers
        .get(SENDER_HEADER)
        .map(|word| Sender::from_word(word.as_bytes()));
    let Some(axum::Extension(axum::extract::ConnectInfo(client))) = connection else {
        return forwarded.unwrap_or(Sender::Person);
    };
    let own = match client.peer().await {
        Peer::Unknown => Sender::Unknown,
        Peer::Itself => {
            let own = if OWN_REQUESTS_ARE_A_PERSONS.load(std::sync::atomic::Ordering::Relaxed) {
                Sender::Person
            } else {
                Sender::Unknown
            };
            return forwarded.unwrap_or(own);
        }
        Peer::Program { agent: true, .. } => Sender::Agent,
        Peer::Program { inside: true, .. } => Sender::Terminal,
        // Another computer, one where the program cannot be told, or a program outside Haarness.
        Peer::Elsewhere | Peer::Untold | Peer::Program { .. } => Sender::Person,
    };
    own.max(forwarded.unwrap_or(Sender::Person))
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
    async fn a_forwarders_word_counts_only_from_this_process_and_only_ever_stricter() {
        let address: SocketAddr = "127.0.0.1:50123".parse().unwrap();
        let judge = |peer: Peer, word: Option<&'static str>| async move {
            let mut headers = HeaderMap::new();
            if let Some(word) = word {
                headers.insert(SENDER_HEADER, HeaderValue::from_static(word));
            }
            let connection = Some(axum::Extension(axum::extract::ConnectInfo(Client::known(
                address, peer,
            ))));
            sender(&connection, &headers).await
        };
        let program = |inside, agent| Peer::Program { inside, agent };
        // This process forwarding: its word, and unknown without one.
        assert_eq!(judge(Peer::Itself, Some("person")).await, Sender::Person);
        assert_eq!(judge(Peer::Itself, Some("agent")).await, Sender::Agent);
        assert_eq!(judge(Peer::Itself, None).await, Sender::Unknown);
        // Anyone else: what its own program says, made stricter by a word but never laxer.
        assert_eq!(
            judge(program(true, true), Some("person")).await,
            Sender::Agent
        );
        assert_eq!(
            judge(program(false, false), Some("agent")).await,
            Sender::Agent
        );
        assert_eq!(judge(program(false, false), None).await, Sender::Person);
        assert_eq!(judge(program(true, false), None).await, Sender::Terminal);
        assert_eq!(
            judge(Peer::Elsewhere, Some("unknown")).await,
            Sender::Unknown
        );
        assert_eq!(judge(Peer::Elsewhere, Some("??")).await, Sender::Unknown);
        assert_eq!(judge(Peer::Untold, None).await, Sender::Person);
        assert_eq!(judge(Peer::Unknown, Some("person")).await, Sender::Unknown);
        // Made inside this server, with no connection.
        assert_eq!(sender(&None, &HeaderMap::new()).await, Sender::Person);
        assert!(Sender::Terminal.inside() && !Sender::Person.inside());
    }

    #[test]
    fn modes_rank_by_what_agents_may_do_unasked() {
        assert!(reach(ApprovalMode::Review) < reach(ApprovalMode::Auto));
        assert!(reach(ApprovalMode::Auto) < reach(ApprovalMode::Yolo));
    }
}
