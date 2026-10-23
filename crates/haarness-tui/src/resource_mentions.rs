//! MCP resources in the `@` list. The attached server's MCP servers are
//! asked for theirs the first time the list opens, off the event loop, each
//! server on its own, and the answer is kept for a minute; never at startup.
//! A server the chat's rules take away entirely is left out.
//!
//! A picked resource is a chip whose draft text is a marker naming it and
//! the pick (a nonce this TUI keeps), so drafts, history and undo keep it
//! like any other reference. Only the TUI's own sends turn a marker it knows
//! into the request's `resources`, which the server reads into the message;
//! anything else (a prompt piped to `haarness run`, an editor's text over
//! ACP, a scheduled prompt, a marker this TUI did not make) is sent as the
//! text it is.
use crate::{api::ApiClient, state::App};
use anyhow::{Result, bail};
use haarness_protocol::{RESOURCE_MENTIONS_CAPABILITY, ResourceMention, ThreadId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// The most resource rows the `@` list shows, after agents and files.
pub(crate) const ROWS: usize = 50;
const TTL: Duration = Duration::from_secs(60);
/// A resource's suggestion: this, then its server and URI.
const KEY: &str = "mcp-resource\u{1f}";
const SUMMARY: &str = "<details>\n<summary>MCP resource: ";
/// Why the server left a mention unread when a rule keeps it from the chat.
pub(crate) const RULE_REFUSAL: &str = "a rule keeps this resource from the model";

/// One resource the `@` list can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resource {
    pub server: String,
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
}

/// The attached server's resources for the `@` list, in the order their
/// servers answered, and the picks this TUI made.
#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub list: Vec<Resource>,
    /// When the listing in progress (or the last one) was asked for.
    pub asked: Option<Instant>,
    /// The chat the servers were asked about, and those it cannot reach.
    pub chat: Option<ThreadId>,
    pub turned_off: HashSet<String>,
    /// No row of the listing asked at `asked` has come yet: the earlier
    /// rows stay until one does.
    pub stale: bool,
    /// Picks by nonce. Only a marker naming one of these is read at send.
    pub picked: HashMap<String, ResourceMention>,
    /// A message whose mentions the server is reading, sent off the loop.
    pub sending: bool,
}

/// What a listing off the event loop reports, tagged with when it was asked.
#[derive(Debug)]
pub(crate) enum Update {
    /// Which servers the chat cannot reach.
    Servers {
        asked: Instant,
        chat: ThreadId,
        turned_off: Vec<String>,
    },
    /// One server's rows.
    Rows { asked: Instant, rows: Vec<Resource> },
    /// The servers could not be listed: the next `@` asks again.
    Failed { asked: Instant },
}

/// A message with mentions sent off the event loop, and how it went.
pub(crate) struct Sent {
    pub chat: ThreadId,
    pub draft: String,
    pub attachments: Vec<String>,
    pub result: Result<Outcome>,
}
pub(crate) enum Outcome {
    Started(haarness_protocol::Turn),
    Steered(haarness_protocol::QueueInputResponse),
    Queued(haarness_protocol::QueueInputResponse),
}

/// Whether the attached server reads resource mentions; an older one would
/// drop them without a word, so it is offered none.
fn offered(app: &App) -> bool {
    app.server_info.as_ref().is_some_and(|info| {
        info.capabilities
            .iter()
            .any(|capability| capability == RESOURCE_MENTIONS_CAPABILITY)
    })
}

/// One server's listing as rows; an entry without a URI is left out and a
/// name falls back to the URI.
fn rows(server: &str, listed: &[serde_json::Value]) -> Vec<Resource> {
    let text = |entry: &serde_json::Value, field: &str| {
        entry
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    listed
        .iter()
        .filter_map(|entry| {
            let uri = text(entry, "uri")?;
            Some(Resource {
                server: server.to_owned(),
                name: text(entry, "name").unwrap_or_else(|| uri.clone()),
                description: text(entry, "description").or_else(|| text(entry, "title")),
                uri,
            })
        })
        .collect()
}

/// Lists the servers for `chat`, then each connected one on its own,
/// sending what comes as it comes.
async fn list(
    client: ApiClient,
    chat: ThreadId,
    asked: Instant,
    only_servers: bool,
    sender: tokio::sync::mpsc::Sender<Update>,
) {
    let Ok(servers) = client.mcp_for(chat).await else {
        let _ = sender.send(Update::Failed { asked }).await;
        return;
    };
    let turned_off: Vec<String> = servers
        .iter()
        .filter(|server| server.turned_off == Some(true))
        .map(|server| server.name.clone())
        .collect();
    let _ = sender
        .send(Update::Servers {
            asked,
            chat,
            turned_off,
        })
        .await;
    if only_servers {
        return;
    }
    // Every connected server, so the listing serves every chat; the rows of
    // one this chat cannot reach are left out when shown.
    for server in servers
        .into_iter()
        .filter(|server| server.status == "connected")
    {
        let (client, sender) = (client.clone(), sender.clone());
        tokio::spawn(async move {
            // One that fails lists nothing; the others are not held up.
            let rows = client
                .mcp_resources(&server.name)
                .await
                .map(|listed| rows(&server.name, &listed))
                .unwrap_or_default();
            let _ = sender.send(Update::Rows { asked, rows }).await;
        });
    }
}

/// The resource rows for what follows `@`: matched by name only, each
/// server's rows in the order the servers answered (so rows that come later
/// go below, and the highlighted row stays put), names that start with it
/// first within a server, at most [`ROWS`]. None while a queued message is
/// edited (its edit saves text only), and none of a server the chat cannot
/// reach.
pub(crate) fn suggestions(app: &App, query: &str) -> Vec<String> {
    let listing = &app.mention_resources;
    if !offered(app) || app.queue_edit.is_some() {
        return Vec::new();
    }
    let query = query.to_lowercase();
    let mut servers: Vec<&str> = Vec::new();
    for resource in &listing.list {
        if !servers.contains(&resource.server.as_str()) {
            servers.push(&resource.server);
        }
    }
    let mut shown = Vec::new();
    for server in servers {
        if listing.turned_off.contains(server) {
            continue;
        }
        let (mut starts, mut holds) = (Vec::new(), Vec::new());
        for resource in listing.list.iter().filter(|row| row.server == server) {
            let name = resource.name.to_lowercase();
            if name.starts_with(&query) {
                starts.push(resource);
            } else if name.contains(&query) {
                holds.push(resource);
            }
        }
        shown.extend(starts.into_iter().chain(holds));
    }
    shown
        .into_iter()
        .take(ROWS)
        .map(|resource| format!("{KEY}{}\u{1f}{}", resource.server, resource.uri))
        .collect()
}

/// The resource a suggestion names.
pub(crate) fn find<'a>(app: &'a App, suggestion: &str) -> Option<&'a Resource> {
    let (server, uri) = suggestion.strip_prefix(KEY)?.split_once('\u{1f}')?;
    app.mention_resources
        .list
        .iter()
        .find(|resource| resource.server == server && resource.uri == uri)
}

/// How a resource row reads: its name, then its description (or URI) and
/// the server it comes from.
pub(crate) fn row(resource: &Resource) -> (String, String) {
    (
        format!("@{}", resource.name),
        format!(
            "{} · {}",
            resource.description.as_deref().unwrap_or(&resource.uri),
            resource.server
        ),
    )
}

/// What a marker holds: the mention and the pick it came from.
#[derive(Serialize, Deserialize)]
struct Marked {
    #[serde(flatten)]
    mention: ResourceMention,
    pick: String,
}

/// The draft text of a picked resource: a block under the label the sent
/// one will have, holding the mention and its pick.
fn marker(mention: &ResourceMention, pick: &str) -> String {
    let address = format!("{}:{}", mention.server, mention.uri);
    let label = match mention.name.as_deref() {
        Some(name) if name != mention.uri => format!("{name} · {address}"),
        _ => address,
    };
    let label = label
        .replace(['\n', '\r'], " ")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let marked = Marked {
        mention: mention.clone(),
        pick: pick.to_owned(),
    };
    format!(
        "{SUMMARY}{label}</summary>\n\n{}\n\n</details>",
        serde_json::to_string(&marked).unwrap_or_default()
    )
}

/// The mention a reference's text is, with its pick, when it is a marker
/// rather than what the server read for one.
fn marked(body: &str) -> Option<Marked> {
    let inner = body
        .strip_prefix(SUMMARY)?
        .split_once("</summary>\n\n")?
        .1
        .strip_suffix("\n\n</details>")?;
    serde_json::from_str(inner).ok()
}

/// A message's text with each marker of a pick in `picked` back to the
/// `@name` it was picked as, and those mentions. A marker of no pick this
/// TUI made stays the text it is.
pub(crate) fn take(
    text: &str,
    picked: &HashMap<String, ResourceMention>,
) -> (String, Vec<ResourceMention>) {
    let mut kept = String::new();
    let mut mentions = Vec::new();
    for part in crate::prompt_context::parts(text) {
        match part {
            crate::prompt_context::Part::Reference { body, .. } => {
                match marked(body)
                    .filter(|marked| picked.get(&marked.pick) == Some(&marked.mention))
                {
                    Some(marked) => {
                        kept.push('@');
                        kept.push_str(
                            marked
                                .mention
                                .name
                                .as_deref()
                                .unwrap_or(&marked.mention.uri),
                        );
                        if !mentions.contains(&marked.mention) {
                            mentions.push(marked.mention);
                        }
                    }
                    None => kept.push_str(body),
                }
            }
            crate::prompt_context::Part::Text(text)
            | crate::prompt_context::Part::Paste { body: text, .. } => kept.push_str(text),
        }
    }
    if mentions.is_empty() {
        return (text.to_owned(), mentions);
    }
    (kept, mentions)
}

impl App {
    /// Asks for the resources when the `@` list opens and no answer younger
    /// than a minute is kept or on its way; for another chat than the one
    /// they were listed for, only which servers it cannot reach.
    pub(crate) fn ask_resources(&mut self, client: &ApiClient) {
        if !offered(self) {
            return;
        }
        let Some(sender) = self.resources_tx.clone() else {
            return;
        };
        let fresh = self
            .mention_resources
            .asked
            .is_some_and(|asked| asked.elapsed() < TTL);
        let chat = self.thread.id;
        if fresh && self.mention_resources.chat == Some(chat) {
            return;
        }
        let asked = if fresh {
            self.mention_resources.asked.unwrap_or_else(Instant::now)
        } else {
            let now = Instant::now();
            self.mention_resources.asked = Some(now);
            self.mention_resources.stale = true;
            now
        };
        self.mention_resources.chat = Some(chat);
        tokio::spawn(list(client.clone(), chat, asked, fresh, sender));
    }

    /// Part of a listing asked off the loop has come. The open `@` list
    /// gains a server's rows below the others, so the row the cursor is on
    /// stays put. A failed listing is asked for again at the next `@`.
    pub(crate) fn resources_arrived(&mut self, update: Update) {
        let listing = &mut self.mention_resources;
        match update {
            Update::Servers {
                asked,
                chat,
                turned_off,
            } if listing.asked == Some(asked) && listing.chat == Some(chat) => {
                listing.turned_off = turned_off.into_iter().collect();
            }
            Update::Rows { asked, rows } if listing.asked == Some(asked) => {
                if listing.stale {
                    listing.list.clear();
                    listing.stale = false;
                }
                listing.list.extend(rows);
            }
            Update::Failed { asked } if listing.asked == Some(asked) => {
                listing.asked = None;
                listing.chat = None;
            }
            _ => return,
        }
        if self.shell_mode || self.composer.text().starts_with('/') {
            return;
        }
        let Some((_, query)) = self.composer.mention_query() else {
            return;
        };
        let rows = suggestions(self, &query);
        let first = self
            .mention_suggestions
            .iter()
            .position(|suggestion| suggestion.starts_with(KEY))
            .unwrap_or(self.mention_suggestions.len());
        let kept = &self.mention_suggestions[first..];
        // Rows already shown keep their places; new ones go below.
        if rows.starts_with(kept) {
            let new = rows[kept.len()..].to_vec();
            self.mention_suggestions.extend(new);
        } else {
            self.mention_suggestions.truncate(first);
            self.mention_suggestions.extend(rows);
        }
    }

    /// Picks the resource row `suggestion` names: it becomes a chip in
    /// place of the `@` query, bound to this pick. False when it names no
    /// resource.
    pub(crate) fn attach_resource(&mut self, suggestion: &str) -> bool {
        let Some(resource) = find(self, suggestion).cloned() else {
            return false;
        };
        let Some((start, query)) = self.composer.mention_query() else {
            return false;
        };
        let mention = ResourceMention {
            server: resource.server,
            uri: resource.uri,
            name: Some(resource.name.clone()),
        };
        let pick = uuid::Uuid::now_v7().simple().to_string();
        let label = format!("@{}", resource.name);
        match self.composer.attach_context(
            start,
            start + 1 + query.len(),
            &label,
            &marker(&mention, &pick),
        ) {
            Ok(()) => {
                self.mention_resources.picked.insert(pick, mention);
                self.notice = format!("Attached {label} · Backspace removes the reference");
            }
            Err(error) => self.notice = error.to_string(),
        }
        self.mention_suggestions.clear();
        self.selection = 0;
        true
    }

    /// A prompt this TUI sends, with its picks as mentions. A server that
    /// cannot read them is told nothing and the draft stays, as in Desktop.
    pub(crate) fn take_mentions(&self, text: &str) -> Result<(String, Vec<ResourceMention>)> {
        let (sent, mentions) = take(text, &self.mention_resources.picked);
        if mentions.is_empty() {
            return Ok((text.to_owned(), mentions));
        }
        if !offered(self) {
            bail!(
                "This server cannot read MCP resources. Remove them from the message, or update the server."
            );
        }
        Ok((sent, mentions))
    }

    /// Whether a message with mentions is still on its way: the next one
    /// waits, so the two cannot overtake each other. Says so when it is.
    pub(crate) fn still_sending(&mut self) -> bool {
        if self.mention_resources.sending {
            self.notice = "Still reading the resources of your last message".into();
        }
        self.mention_resources.sending
    }

    /// Sends a message with mentions off the event loop, so the prompt
    /// stays live while the server reads them (at most 10 s). Its
    /// attachments go with it and come back if it fails.
    pub(crate) async fn send_with_mentions(
        &mut self,
        draft: String,
        mentions: &[ResourceMention],
        send: impl std::future::Future<Output = Result<Outcome>> + Send + 'static,
    ) {
        let attachments = std::mem::take(&mut self.attachments);
        let chat = self.thread.id;
        self.notice = match mentions {
            [one] => format!("Reading @{}…", one.name.as_deref().unwrap_or(&one.uri)),
            many => format!("Reading {} MCP resources…", many.len()),
        };
        let Some(sender) = self.sends_tx.clone() else {
            let result = send.await;
            self.mentions_sent(
                Sent {
                    chat,
                    draft,
                    attachments,
                    result,
                },
                None,
            )
            .await;
            return;
        };
        self.mention_resources.sending = true;
        tokio::spawn(async move {
            let result = send.await;
            let _ = sender
                .send(Sent {
                    chat,
                    draft,
                    attachments,
                    result,
                })
                .await;
        });
    }

    /// A message sent with mentions was admitted, or was not: then its draft
    /// and attachments come back, to this chat's prompt or its saved draft.
    pub(crate) async fn mentions_sent(&mut self, sent: Sent, client: Option<&ApiClient>) {
        self.mention_resources.sending = false;
        let here = sent.chat == self.thread.id;
        match sent.result {
            Ok(_) if !here => self.refresh_needed = true,
            Ok(Outcome::Started(turn)) => self.turn_sent(turn),
            Ok(Outcome::Steered(response)) => self.steer_sent(response),
            Ok(Outcome::Queued(response)) => {
                if let Some(turn) = response.turn {
                    self.active_turn = Some(turn.id);
                }
                if let Some(client) = client
                    && let Ok(queue) = client.input_queue(self.thread.id).await
                {
                    self.input_queue = queue;
                }
                self.refresh_needed = true;
                self.notice = "Queued after current work · click it to edit, send or remove".into();
            }
            Err(error) => {
                if here && self.composer.is_empty() && self.attachments.is_empty() {
                    self.composer.set_text(sent.draft);
                    self.attachments = sent.attachments;
                } else if !here {
                    self.drafts
                        .entry(sent.chat)
                        .or_insert(crate::servers::Draft {
                            text: sent.draft,
                            attachments: sent.attachments,
                            shell: false,
                        });
                }
                self.notice = "Ready".into();
                self.show_toast(
                    Some("Not sent".into()),
                    error.to_string(),
                    crate::state::ToastVariant::Error,
                    Duration::from_secs(8),
                );
            }
        }
    }
}

/// Why a mention was not read, in the person's language when Haarness
/// wrote the reason (an MCP server's own words stay as they are).
pub(crate) fn unread_reason(language: crate::locale::Language, reason: &str) -> String {
    if reason == RULE_REFUSAL {
        return language.text(RULE_REFUSAL).to_owned();
    }
    if let Some(seconds) = reason
        .strip_prefix("did not answer within ")
        .and_then(|rest| rest.strip_suffix(" s"))
    {
        return language.format("did not answer within {seconds} s", &[("seconds", seconds)]);
    }
    reason.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(server: &str, uri: &str, name: &str) -> Resource {
        Resource {
            server: server.into(),
            uri: uri.into(),
            name: name.into(),
            description: None,
        }
    }

    fn mention(server: &str, uri: &str, name: &str) -> ResourceMention {
        ResourceMention {
            server: server.into(),
            uri: uri.into(),
            name: Some(name.into()),
        }
    }

    fn app() -> App {
        let thread = haarness_protocol::Thread {
            schedule: None,
            id: haarness_protocol::ThreadId::new(),
            title: "Test".into(),
            parent_id: None,
            is_subagent: false,
            fork_origin: None,
            archived: false,
            agent: "build".into(),
            provider: "echo".into(),
            model: "echo-1".into(),
            ultracode: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            revert: None,
        };
        App::new(thread.clone(), vec![thread], vec![], vec![], vec![])
    }

    fn info(capabilities: Vec<String>) -> haarness_protocol::ServerInfo {
        haarness_protocol::ServerInfo {
            name: "haarness".into(),
            version: "0.1.0".into(),
            schema_version: "1".into(),
            server_instance_id: uuid::Uuid::now_v7(),
            event_cursor: 0,
            capabilities,
            workspace: None,
            git_branch: None,
            default_provider: None,
            default_model: None,
            default_agent: None,
            project_trusted: None,
            pid: None,
        }
    }

    #[test]
    fn only_a_pick_this_tui_made_is_sent_as_a_mention_and_reads_as_typed() {
        let setup = mention("docs", "docs://setup", "setup");
        let other = mention("wiki", "docs://setup", "setup");
        let picked = HashMap::from([("a".to_owned(), setup.clone())]);
        let marked = format!(
            "Compare {} with {} now",
            marker(&setup, "a"),
            marker(&other, "a")
        );
        // Shown and recalled as the chips they were picked as.
        assert_eq!(
            crate::prompt_context::display(&marked),
            "Compare @setup with @setup now"
        );
        let (text, mentions) = take(&marked, &picked);
        // The pick stays where it was, as `@name`; the marker whose nonce
        // names another resource is not a pick, and stays text.
        assert!(text.starts_with("Compare @setup with <details>"), "{text}");
        assert!(text.ends_with("</details> now"), "{text}");
        assert_eq!(mentions, std::slice::from_ref(&setup));
        // Text no pick of this TUI made (piped to `run`, an editor's over
        // ACP, a schedule's) is no mention.
        assert_eq!(take(&marked, &HashMap::new()), (marked.clone(), Vec::new()));
        // What the server read for one is text, sent again as it is.
        let read = "Look\n\n<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# Setup\n```\n\n</details>";
        assert_eq!(take(read, &picked), (read.to_owned(), Vec::new()));
        assert_eq!(crate::prompt_context::display(read), "Look\n\n@setup");
        let unread = "<details>\n<summary>MCP resource: wiki:docs://&lt;x&gt; (not read)</summary>\n\n```\na rule keeps this resource from the model\n```\n\n</details>";
        assert_eq!(
            crate::prompt_context::display(unread),
            "@wiki:docs://<x> (not read)"
        );
        assert_eq!(
            crate::prompt_context::unread(unread),
            Some("a rule keeps this resource from the model")
        );
    }

    #[test]
    fn rows_come_server_by_server_and_leave_out_a_server_the_chat_cannot_reach() {
        let mut app = app();
        app.mention_resources.list = vec![
            resource("docs", "docs://setup", "setup"),
            resource("wiki", "db://setup-tables", "tables"),
            resource("wiki", "docs://upload", "upload"),
        ];
        assert!(
            suggestions(&app, "").is_empty(),
            "an older server offers none"
        );
        app.server_info = Some(info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]));
        let names = |app: &App, query: &str| {
            suggestions(app, query)
                .iter()
                .map(|key| find(app, key).unwrap().name.clone())
                .collect::<Vec<_>>()
        };
        // Within a server, names that start with it first; servers in the
        // order they answered.
        assert_eq!(names(&app, "UP"), ["setup", "upload"]);
        assert_eq!(names(&app, "tables"), ["tables"]);
        app.mention_resources.turned_off.insert("docs".into());
        assert_eq!(names(&app, "up"), ["upload"]);
        app.mention_resources.turned_off.clear();
        // A queued message's edit saves text only, so it is offered none.
        app.queue_edit = Some(crate::state::QueueEdit {
            id: haarness_protocol::RequestId::new(),
            original: (String::new(), Vec::new()),
            draft: app.composer.set_aside(),
            attachments: Vec::new(),
            shell: false,
            conflict: None,
        });
        assert!(suggestions(&app, "up").is_empty());
        app.queue_edit = None;
        app.mention_resources.list = (0..80)
            .map(|index| resource("docs", &format!("docs://{index}"), &format!("page {index}")))
            .collect();
        assert_eq!(names(&app, "").len(), ROWS);
        assert_eq!(
            row(&resource("docs", "docs://logo", "logo")),
            ("@logo".to_owned(), "docs://logo · docs".to_owned())
        );
        assert_eq!(
            rows(
                "docs",
                &[
                    serde_json::json!({"uri": "docs://setup", "name": "setup", "title": "Setup guide"}),
                    serde_json::json!({"name": "no address"}),
                ]
            ),
            [Resource {
                server: "docs".into(),
                uri: "docs://setup".into(),
                name: "setup".into(),
                description: Some("Setup guide".into()),
            }]
        );
    }

    #[test]
    fn rows_join_the_open_list_below_and_a_pick_is_a_chip_sent_as_a_mention() {
        let mut app = app();
        app.server_info = Some(info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]));
        let asked = Instant::now();
        app.mention_resources.asked = Some(asked);
        app.mention_resources.chat = Some(app.thread.id);
        app.composer.set_text("Compare @se".into());
        app.mention_suggestions = vec!["src/setup.rs".into()];
        app.selection = 1;
        app.resources_arrived(Update::Rows {
            asked,
            rows: vec![resource("wiki", "docs://setup", "setup")],
        });
        // A slower server's rows go below; the highlighted row stays put.
        app.resources_arrived(Update::Rows {
            asked,
            rows: vec![
                resource("docs", "docs://setup", "setup"),
                resource("docs", "docs://logo", "logo"),
            ],
        });
        let listed = app.mention_suggestions.clone();
        assert_eq!(listed.len(), 3, "{listed:?}");
        assert_eq!(listed[0], "src/setup.rs");
        assert_eq!(find(&app, &listed[1]).unwrap().server, "wiki");
        assert_eq!(find(&app, &listed[2]).unwrap().server, "docs");
        assert_eq!(app.selection, 1);
        // A late answer of an older listing changes nothing.
        app.resources_arrived(Update::Rows {
            asked: asked.checked_sub(Duration::from_secs(1)).unwrap(),
            rows: vec![resource("old", "x://y", "setup")],
        });
        assert_eq!(app.mention_suggestions, listed);
        assert!(app.attach_resource(&listed[1]));
        assert_eq!(app.composer.text(), "Compare [@setup] ");
        assert!(app.notice.starts_with("Attached @setup"), "{}", app.notice);
        let draft = app.composer.take();
        let (text, mentions) = app.take_mentions(&draft).unwrap();
        assert_eq!(text, "Compare @setup ");
        assert_eq!(mentions, [mention("wiki", "docs://setup", "setup")]);
        // History brings the chip back, naming the same pick.
        app.composer.set_text(draft.clone());
        assert_eq!(app.composer.text(), "Compare [@setup] ");
        assert_eq!(
            app.take_mentions(&app.composer.expanded_text())
                .unwrap()
                .1
                .len(),
            1
        );
        // A server that cannot read them keeps the draft, as Desktop does.
        app.server_info = Some(info(Vec::new()));
        assert!(app.take_mentions(&draft).is_err());
        // A listing that failed is asked for again at the next `@`.
        app.resources_arrived(Update::Failed { asked });
        assert!(app.mention_resources.asked.is_none());
    }

    /// A server that answers `{}` to `count` requests and hands back their
    /// bodies.
    async fn capture(count: usize) -> (String, tokio::task::JoinHandle<Vec<serde_json::Value>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for _ in 0..count {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                let body = loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                        continue;
                    };
                    let length = String::from_utf8_lossy(&request[..end])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break request[end + 4..end + 4 + length].to_vec();
                    }
                };
                bodies.push(serde_json::from_slice(&body).unwrap_or_default());
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                    .await;
            }
            bodies
        });
        (format!("http://{address}"), server)
    }

    /// `haarness run`, an editor over ACP and a scheduled run send their
    /// prompts through the same client the TUI does: whatever the text
    /// holds, even a marker of a pick, it goes as text and nothing is read.
    #[tokio::test]
    async fn the_client_sends_any_text_as_it_is_and_never_reads_from_it() {
        let setup = mention("docs", "docs://setup", "setup");
        let text = format!("Summarise {} please", marker(&setup, "nonce"));
        let (url, server) = capture(3).await;
        let client = ApiClient::new(&url, "token").unwrap();
        let thread = haarness_protocol::ThreadId::new();
        let _ = client
            .start_turn(
                thread,
                haarness_protocol::StartTurnRequest {
                    request_id: haarness_protocol::RequestId::new(),
                    text: text.clone(),
                    attachments: Vec::new(),
                    resources: Vec::new(),
                    synthetic: false,
                    auto_approve: false,
                    agent: None,
                    provider: None,
                    model: None,
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                },
            )
            .await;
        let _ = client
            .queue_input(
                thread,
                haarness_protocol::QueueInputRequest {
                    preview: None,
                    reasoning_effort: None,
                    delivery: haarness_protocol::InputDelivery::Steer,
                    request_id: haarness_protocol::RequestId::new(),
                    text: text.clone(),
                    attachments: Vec::new(),
                    resources: Vec::new(),
                    synthetic: false,
                },
            )
            .await;
        let _ = client
            .execute_command(
                thread,
                haarness_protocol::ExecuteCommandRequest {
                    request_id: haarness_protocol::RequestId::new(),
                    name: "review".into(),
                    arguments: Some(serde_json::Value::String(text.clone())),
                    attachments: Vec::new(),
                    resources: Vec::new(),
                    agent: None,
                    provider: None,
                    model: None,
                    variant: None,
                    reasoning_effort: None,
                    tools: None,
                    wait: false,
                    timeout_ms: None,
                },
            )
            .await;
        let bodies = server.await.unwrap();
        assert_eq!(bodies.len(), 3);
        for body in &bodies {
            assert!(body.get("resources").is_none(), "{body}");
            assert_eq!(
                body.get("text").or_else(|| body.get("arguments")),
                Some(&serde_json::Value::String(text.clone())),
                "{body}"
            );
        }
    }
}
