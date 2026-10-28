//! The names tools have on the wire to a provider.
//!
//! Inside Haarness a tool keeps its own name: an MCP tool is
//! `mcp__<server>__<tool>` with the server's and the tool's names as they are
//! written, and rules, settings, hooks, workflows, transcripts and every
//! client use that name. Providers check every tool name of a request and
//! refuse the whole request over one they cannot take: OpenAI, Anthropic and
//! Bedrock accept letters, digits, `_` and `-`, at most 64 of them. A server
//! named `Docs Server`, or a tool named `files.read` (MCP allows dots), would
//! otherwise fail every turn while that server is connected.
//!
//! So each request to a provider gives such a tool a name of those characters
//! for that request ([`WireNames`]): its other characters as `_`, and when
//! that name is longer than 64, or another tool of the request (or of the
//! catalog the request was chosen from) already has it, its start and a short
//! hash of its own name. Tool calls the provider streams back, and the names
//! the model writes elsewhere (Code Mode, `batch`, `load_tool`), are read
//! back to the tools' own names. A name providers already accept is sent as
//! it is, so a request whose tools all have such names is unchanged.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use futures::StreamExt as _;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    CoreResult, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream,
};

/// The longest tool name every provider takes.
pub const MAX_NAME: usize = 64;
/// Hex characters of the hash a name ends with when its own spelling cannot
/// be used.
const HASH: usize = 8;

/// The names one request gives its tools on the wire, both ways. Empty when
/// every name is one providers take, which is the usual case.
#[derive(Debug, Default)]
pub struct WireNames {
    wire: HashMap<String, String>,
    own: HashMap<String, String>,
}

impl WireNames {
    /// Wire names for `names`, the tools a request may hold (a catalog). The
    /// same set of names always gets the same wire names, so a name the
    /// model wrote can be read back from the catalog alone.
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut taken = HashSet::new();
        let mut changed = BTreeSet::new();
        for name in names {
            if accepted(name) {
                taken.insert(name.to_owned());
            } else {
                changed.insert(name);
            }
        }
        let mut names = Self::default();
        for name in changed {
            names.assign(name, &mut taken);
        }
        names
    }

    /// Wire names for `request`'s tools and the tool calls and results of its
    /// history. Only when one of them needs another name are `catalog`'s
    /// names (the tools the request's were chosen from) read too: the
    /// request's tools are named as [`new`](Self::new) names that catalog,
    /// so a tool held back from this request keeps its name from another's,
    /// and a tool of the history that is no longer offered takes a name no
    /// tool of the catalog has.
    pub fn for_request<'a, C>(request: &'a ProviderRequest, catalog: impl FnOnce() -> C) -> Self
    where
        C: IntoIterator<Item = &'a str>,
    {
        if request_names(request).all(accepted) {
            return Self::default();
        }
        let tools: HashSet<&str> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .chain(catalog())
            .collect();
        let mut names = Self::new(tools.iter().copied());
        let mut taken: HashSet<String> = tools
            .iter()
            .map(|name| names.wire(name).to_owned())
            .collect();
        let history: BTreeSet<&str> = request_names(request)
            .filter(|name| !tools.contains(name))
            .collect();
        for name in history {
            if accepted(name) && !taken.contains(name) {
                taken.insert(name.to_owned());
            } else {
                names.assign(name, &mut taken);
            }
        }
        names
    }

    /// Give `name` the first wire name `taken` does not hold.
    fn assign(&mut self, name: &str, taken: &mut HashSet<String>) {
        let plain: String = name.chars().map(kept).collect();
        let wire = if !plain.is_empty() && plain.len() <= MAX_NAME && !taken.contains(&plain) {
            plain
        } else {
            hashed(name, &plain, taken)
        };
        taken.insert(wire.clone());
        self.own.insert(wire.clone(), name.to_owned());
        self.wire.insert(name.to_owned(), wire);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.wire.is_empty()
    }

    /// The name tool `name` has on the wire.
    #[must_use]
    pub fn wire<'a>(&'a self, name: &'a str) -> &'a str {
        self.wire.get(name).map_or(name, String::as_str)
    }

    /// The tool a name the provider or the model wrote stands for: its own
    /// name for a wire name of this request (also after a `functions.`
    /// prefix some models add), else the name as written.
    #[must_use]
    pub fn own<'a>(&'a self, name: &'a str) -> &'a str {
        if let Some(own) = self.own.get(name) {
            return own;
        }
        name.get(..10)
            .filter(|prefix| prefix.eq_ignore_ascii_case("functions."))
            .and_then(|_| self.own.get(&name[10..]))
            .map_or(name, String::as_str)
    }

    /// `mask` (tool names or patterns to a value, as a workflow agent's or
    /// an agent's `tools`) with each key that is a wire name under the tool's
    /// own name; a key written as that own name wins. Patterns are kept as
    /// written: they match the tools' own names.
    pub fn own_keys<V>(&self, mask: impl IntoIterator<Item = (String, V)>) -> Vec<(String, V)> {
        let (mut keys, own): (Vec<_>, Vec<_>) = mask
            .into_iter()
            .partition(|(key, _)| self.own.contains_key(key));
        for (key, _) in &mut keys {
            key.clone_from(&self.own[key.as_str()]);
        }
        keys.extend(own);
        keys
    }

    /// `request` with its tools, and the calls and results of its history,
    /// under their wire names.
    #[must_use]
    pub fn applied(&self, mut request: ProviderRequest) -> ProviderRequest {
        if self.is_empty() {
            return request;
        }
        for tool in &mut request.tools {
            self.rename(&mut tool.name);
        }
        for message in &mut request.messages {
            if let ProviderMessage::ToolCall { name, .. }
            | ProviderMessage::ToolResult { name, .. } = message
            {
                self.rename(name);
            }
        }
        request
    }

    /// `stream` with the tool calls it carries under the tools' own names.
    #[must_use]
    pub fn read_back(self: Arc<Self>, stream: ProviderStream) -> ProviderStream {
        if self.is_empty() {
            return stream;
        }
        Box::pin(stream.map(move |event| match event {
            Ok(ProviderEvent::ToolCall {
                call_id,
                name,
                arguments,
                provider_metadata,
            }) => Ok(ProviderEvent::ToolCall {
                call_id,
                name: self.own(&name).to_owned(),
                arguments,
                provider_metadata,
            }),
            other => other,
        }))
    }

    fn rename(&self, name: &mut String) {
        if let Some(wire) = self.wire.get(name.as_str()) {
            name.clone_from(wire);
        }
    }
}

/// Stream `request` from `provider` with its tool names on the wire as
/// [`WireNames::for_request`] gives them (from the request alone), and the
/// calls that come back under the tools' own names.
pub async fn stream(
    provider: &dyn ModelProvider,
    request: ProviderRequest,
    cancel: CancellationToken,
) -> CoreResult<ProviderStream> {
    let names = Arc::new(WireNames::for_request(&request, std::iter::empty));
    let stream = provider.stream(names.applied(request), cancel).await?;
    Ok(names.read_back(stream))
}

/// Every tool name `request` holds: its tools' and its history's.
fn request_names(request: &ProviderRequest) -> impl Iterator<Item = &str> {
    request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .chain(request.messages.iter().filter_map(|message| match message {
            ProviderMessage::ToolCall { name, .. } | ProviderMessage::ToolResult { name, .. } => {
                Some(name.as_str())
            }
            _ => None,
        }))
}

/// Whether every provider takes `name` as it is.
#[must_use]
pub fn accepted(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && name.bytes().all(allowed)
}

/// `plain` (`name` with its other characters replaced) cut to leave room for
/// `_` and a hash of `name` that no name in `taken` has.
fn hashed(name: &str, plain: &str, taken: &HashSet<String>) -> String {
    // Every character kept is ASCII, so a byte index is a character's.
    let start = &plain[..plain.len().min(MAX_NAME - HASH - 1)];
    let mut attempt = 0_u32;
    loop {
        let suffix = if attempt == 0 {
            hash(&[name])
        } else {
            hash(&[name, &attempt.to_string()])
        };
        let candidate = format!("{start}_{suffix}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        attempt += 1;
    }
}

/// `character` as a name on the wire holds it: `_` for one providers refuse.
fn kept(character: char) -> char {
    if u8::try_from(character).is_ok_and(allowed) {
        character
    } else {
        '_'
    }
}

fn allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// The first [`HASH`] hex characters of the SHA-256 of `parts`, each ended
/// by a NUL.
fn hash(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    let digest = digest.finalize();
    let mut text = String::with_capacity(HASH);
    for byte in digest.iter().take(HASH / 2) {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolDefinition;
    use serde_json::{Value, json};

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn request(tools: &[&str], history: &[&str]) -> ProviderRequest {
        let mut messages = Vec::new();
        for (index, name) in history.iter().enumerate() {
            let call_id = format!("call-{index}");
            messages.push(ProviderMessage::ToolCall {
                call_id: call_id.clone(),
                name: (*name).into(),
                arguments: json!({}),
                provider_metadata: Value::Null,
            });
            messages.push(ProviderMessage::ToolResult {
                call_id,
                name: (*name).into(),
                output: "done".into(),
                is_error: false,
                attachments: Vec::new(),
            });
        }
        ProviderRequest {
            model: "m".into(),
            system: String::new(),
            messages,
            tools: tools.iter().map(|name| tool(name)).collect(),
            reasoning_effort: None,
            output_schema: None,
            parameters: crate::RequestParameters::default(),
        }
    }

    fn sent_names(request: &ProviderRequest) -> Vec<String> {
        request_names(request).map(str::to_owned).collect()
    }

    #[test]
    fn names_providers_take_are_sent_as_they_are() {
        let names = WireNames::new(["read", "mcp__github__create_issue", "mcp__a__b__c"]);
        assert!(names.is_empty());
        let request = request(&["read", "mcp__github__create_issue"], &["bash"]);
        let names = WireNames::for_request(&request, || -> Vec<&str> {
            panic!("the catalog is read only when a name changes")
        });
        assert!(names.is_empty());
        assert_eq!(
            sent_names(&names.applied(request)),
            ["read", "mcp__github__create_issue", "bash", "bash"]
        );
    }

    #[test]
    fn other_names_go_out_in_characters_providers_take_and_come_back_as_they_were() {
        let names = WireNames::new([
            "mcp__Docs Server__files.read",
            "mcp__srv__what?",
            "mcp__文档__search",
            "read",
        ]);
        for (own, wire) in [
            (
                "mcp__Docs Server__files.read",
                "mcp__Docs_Server__files_read",
            ),
            ("mcp__srv__what?", "mcp__srv__what_"),
            ("mcp__文档__search", "mcp______search"),
            ("read", "read"),
        ] {
            assert_eq!(names.wire(own), wire);
            assert_eq!(names.own(wire), own);
        }
        assert_eq!(
            names.own("functions.mcp__Docs_Server__files_read"),
            "mcp__Docs Server__files.read"
        );
        assert_eq!(names.own("mcp__unknown__tool"), "mcp__unknown__tool");
    }

    /// `Docs Server` and `Docs_Server` (or a project's `google drive`
    /// beside `google_drive`) in one request: the name already accepted
    /// keeps it, the other gets a hash, and each comes back as its own.
    #[test]
    fn two_names_that_come_out_alike_stay_apart() {
        let names = WireNames::new([
            "mcp__Docs Server__plain",
            "mcp__Docs_Server__plain",
            "mcp__Docs.Server__plain",
        ]);
        let wires: HashSet<&str> = [
            "mcp__Docs Server__plain",
            "mcp__Docs_Server__plain",
            "mcp__Docs.Server__plain",
        ]
        .into_iter()
        .map(|own| names.wire(own))
        .collect();
        assert_eq!(wires.len(), 3);
        assert_eq!(
            names.wire("mcp__Docs_Server__plain"),
            "mcp__Docs_Server__plain"
        );
        for own in ["mcp__Docs Server__plain", "mcp__Docs.Server__plain"] {
            let wire = names.wire(own);
            assert!(
                accepted(wire) && wire.starts_with("mcp__Docs_Server__plain_"),
                "{wire}"
            );
            assert_eq!(names.own(wire), own);
        }
        // The same names give the same wire names, whatever their order.
        let again = WireNames::new([
            "mcp__Docs.Server__plain",
            "mcp__Docs_Server__plain",
            "mcp__Docs Server__plain",
        ]);
        assert_eq!(
            again.wire("mcp__Docs Server__plain"),
            names.wire("mcp__Docs Server__plain")
        );
    }

    #[test]
    fn a_name_longer_than_64_is_cut_with_a_hash() {
        let long = format!(
            "mcp__github-enterprise__{}",
            "list_pull_request_review_comments_for_the_whole_repository"
        );
        let other = format!("{long}_again");
        let names = WireNames::new([long.as_str(), other.as_str()]);
        for own in [&long, &other] {
            let wire = names.wire(own);
            assert!(accepted(wire) && wire.len() == MAX_NAME, "{wire}");
            assert_eq!(names.own(wire), own.as_str());
        }
        assert_ne!(names.wire(&long), names.wire(&other));
    }

    /// A tool of the catalog that this request holds back keeps its name, so
    /// the model never reads one tool under another's name; and every tool
    /// of the request has the name the catalog alone gives it, which is how
    /// a name the model writes later is read back.
    #[test]
    fn the_catalog_keeps_its_names_from_a_requests_tools() {
        let catalog = [
            "mcp__Docs Server__plain",
            "mcp__Docs_Server__plain",
            "mcp__Old.Server__plain",
        ];
        // A call of a server no longer offered, whose name comes out as a
        // catalog tool's does.
        let request = request(&["mcp__Docs Server__plain"], &["mcp__Old Server__plain"]);
        let names = WireNames::for_request(&request, || catalog);
        let alone = WireNames::new(catalog);
        assert_ne!(
            names.wire("mcp__Docs Server__plain"),
            "mcp__Docs_Server__plain"
        );
        for tool in catalog {
            assert_eq!(names.wire(tool), alone.wire(tool), "{tool}");
        }
        let old = names.wire("mcp__Old Server__plain");
        assert!(
            accepted(old) && old != alone.wire("mcp__Old.Server__plain"),
            "{old}"
        );
        assert_eq!(names.own(old), "mcp__Old Server__plain");
    }

    /// A chat stored with calls of tools named as providers refuse (a tool
    /// `what?`) goes out with them under wire names, each result beside its
    /// call; the request's tools are renamed alike.
    #[test]
    fn the_history_goes_out_under_the_same_names() {
        let request = request(
            &["mcp__Docs Server__files.read", "read"],
            &["mcp__srv__what?", "mcp__Docs Server__files.read", "bash"],
        );
        let names = WireNames::for_request(&request, std::iter::empty);
        let sent = names.applied(request);
        assert!(
            sent_names(&sent).iter().all(|name| accepted(name)),
            "{:?}",
            sent_names(&sent)
        );
        assert_eq!(
            sent_names(&sent),
            [
                "mcp__Docs_Server__files_read",
                "read",
                "mcp__srv__what_",
                "mcp__srv__what_",
                "mcp__Docs_Server__files_read",
                "mcp__Docs_Server__files_read",
                "bash",
                "bash",
            ]
        );
        for (index, message) in sent.messages.iter().enumerate() {
            let call_id = match message {
                ProviderMessage::ToolCall { call_id, .. }
                | ProviderMessage::ToolResult { call_id, .. } => call_id,
                other => panic!("{other:?}"),
            };
            assert_eq!(call_id, &format!("call-{}", index / 2));
        }
    }

    #[tokio::test]
    async fn a_providers_calls_come_back_under_the_tools_own_names() {
        let names = Arc::new(WireNames::new(["mcp__Docs Server__files.read", "read"]));
        let events = futures::stream::iter(
            [
                "mcp__Docs_Server__files_read",
                "read",
                "functions.mcp__Docs_Server__files_read",
            ]
            .into_iter()
            .map(|name| {
                Ok(ProviderEvent::ToolCall {
                    call_id: name.into(),
                    name: name.into(),
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                })
            }),
        );
        let called: Vec<String> = names
            .read_back(Box::pin(events))
            .map(|event| match event.unwrap() {
                ProviderEvent::ToolCall { name, .. } => name,
                other => panic!("{other:?}"),
            })
            .collect()
            .await;
        assert_eq!(
            called,
            [
                "mcp__Docs Server__files.read",
                "read",
                "mcp__Docs Server__files.read"
            ]
        );
    }

    #[test]
    fn a_mask_keyed_by_wire_names_is_keyed_by_the_tools_own() {
        let names = WireNames::new(["mcp__Docs Server__files.read", "mcp__Docs Server__plain"]);
        let mask: HashMap<String, bool> = names
            .own_keys([
                ("mcp__Docs_Server__files_read".to_owned(), false),
                ("mcp__Docs_Server__plain".to_owned(), false),
                ("mcp__Docs Server__plain".to_owned(), true),
                ("mcp__Docs_Server__*".to_owned(), false),
            ])
            .into_iter()
            .collect();
        assert_eq!(
            mask,
            HashMap::from([
                ("mcp__Docs Server__files.read".to_owned(), false),
                ("mcp__Docs Server__plain".to_owned(), true),
                ("mcp__Docs_Server__*".to_owned(), false),
            ])
        );
    }

    /// A provider that records the names it was sent and calls the first
    /// tool of the request by that name.
    struct Recorder(std::sync::Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl ModelProvider for Recorder {
        fn id(&self) -> &str {
            "recorder"
        }
        fn info(&self) -> haarness_protocol::ProviderInfo {
            haarness_protocol::ProviderInfo {
                id: "recorder".into(),
                name: "Recorder".into(),
                authenticated: true,
                models: Vec::new(),
            }
        }
        async fn stream(
            &self,
            request: ProviderRequest,
            _cancel: CancellationToken,
        ) -> CoreResult<ProviderStream> {
            let names: Vec<String> = request_names(&request).map(str::to_owned).collect();
            let first = names[0].clone();
            *self.0.lock().unwrap() = names;
            Ok(Box::pin(futures::stream::iter([Ok(
                ProviderEvent::ToolCall {
                    call_id: "call".into(),
                    name: first,
                    arguments: json!({}),
                    provider_metadata: Value::Null,
                },
            )])))
        }
    }

    /// Every request a chat sends that is not one of its turns (a side
    /// question, a title, a compaction, Auto's review) goes out the same way.
    #[tokio::test]
    async fn a_request_streamed_through_its_names_goes_out_and_comes_back_translated() {
        let provider = Recorder(std::sync::Mutex::default());
        let sent = request(&["mcp__Docs Server__files.read"], &["mcp__srv__what?"]);
        let called: Vec<String> = stream(&provider, sent, CancellationToken::new())
            .await
            .unwrap()
            .map(|event| match event.unwrap() {
                ProviderEvent::ToolCall { name, .. } => name,
                other => panic!("{other:?}"),
            })
            .collect()
            .await;
        let names = provider.0.lock().unwrap().clone();
        assert!(names.iter().all(|name| accepted(name)), "{names:?}");
        assert_eq!(called, ["mcp__Docs Server__files.read"]);
    }

    /// About 6,000 tools, a few thousand of them with names providers refuse:
    /// one request's names cost a few milliseconds, with no hashing unless
    /// two names come out alike.
    #[test]
    fn a_large_catalog_is_named_quickly() {
        let names: Vec<String> = (0..4)
            .flat_map(|server| {
                (0..1500).map(move |index| {
                    let server = if server % 2 == 1 {
                        format!("big {server}")
                    } else {
                        format!("big{server}")
                    };
                    let tool = if index % 2 == 1 {
                        format!("group.t{index}")
                    } else {
                        format!("t{index}")
                    };
                    format!("mcp__{server}__{tool}")
                })
            })
            .collect();
        let started = std::time::Instant::now();
        let wire = WireNames::new(names.iter().map(String::as_str));
        let took = started.elapsed();
        assert_eq!(wire.wire.len(), 4500);
        assert!(names.iter().all(|name| accepted(wire.wire(name))));
        assert!(took < std::time::Duration::from_millis(500), "{took:?}");
    }
}
