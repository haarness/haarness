//! MCP resources in the `@` list. The attached server's MCP servers are
//! asked for theirs the first time the list opens, off the event loop, each
//! server on its own, and the answer is kept for a minute; never at startup.
//! A server the chat's rules take away entirely is left out.
//!
//! A picked resource is a chip whose draft text is a marker naming it and
//! the pick (a nonce), so drafts, history and undo keep it like any other
//! reference. The TUI keeps its picks next to its history and drafts
//! (`tui-resource-picks.json`), so a chip recalled after a restart still
//! works. Only the TUI's own sends turn a marker of a pick it made into the
//! request's `resources`, which the server reads into the message; a marker
//! of a pick it does not know goes back to the prompt as plain `@name`, and
//! anywhere else (a question's answer, a shell command, a queued message's
//! edit) a marker leaves as `@name`. Text from elsewhere (a prompt piped to
//! `haarness run`, an editor over ACP, a scheduled prompt) is never read.
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
    pub chat: Option<Asking>,
    pub turned_off: HashSet<String>,
    /// No row of the listing asked at `asked` has come yet: the earlier
    /// rows stay until one does.
    pub stale: bool,
    /// Picks by nonce. Only a marker naming one of these is read at send.
    pub picked: HashMap<String, ResourceMention>,
    /// A message whose mentions the server is reading, sent off the loop.
    pub sending: bool,
    /// A draft whose unknown picks became plain `@name` text: what the
    /// prompt gets back instead of the draft that was not sent.
    pub returned: Option<Returned>,
    /// The row a returned pick names: highlighted once its `@` list shows
    /// it, so Enter picks it again even below a file of the same name.
    pub want: Option<String>,
}

/// A draft that comes back instead of being sent, its unknown picks plain
/// `@name` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Returned {
    pub text: String,
    /// Where the first of them ends, for the cursor, so its `@` list opens.
    pub at: usize,
    /// The `@` list row that picks it again.
    pub row: String,
}

/// What decides which servers a chat cannot reach: the chat, the agent its
/// next prompt names, and whether a turn runs there (a message sent then
/// follows that turn's agent and the chat's too).
pub(crate) type Asking = (ThreadId, String, bool);

/// What a listing off the event loop reports, tagged with when it was asked.
#[derive(Debug)]
pub(crate) enum Update {
    /// Which servers the chat cannot reach.
    Servers {
        asked: Instant,
        chat: Asking,
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

/// Lists the servers for `chat` under its agent, then each connected one on
/// its own, sending what comes as it comes.
async fn list(
    client: ApiClient,
    chat: Asking,
    asked: Instant,
    only_servers: bool,
    sender: tokio::sync::mpsc::Sender<Update>,
) {
    let Ok(servers) = client.mcp_for(chat.0, &chat.1).await else {
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
        .map(|resource| key(&resource.server, &resource.uri))
        .collect()
}

/// The `@` list row of the resource `uri` of `server`.
fn key(server: &str, uri: &str) -> String {
    format!("{KEY}{server}\u{1f}{uri}")
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

/// Whether a reference's text is a pick's marker, which is a chip in the
/// prompt but never in the transcript.
pub(crate) fn is_marker(body: &str) -> bool {
    marked(body).is_some()
}

/// How a pick reads as text: `@name`.
fn at_name(mention: &ResourceMention) -> String {
    format!("@{}", mention.name.as_deref().unwrap_or(&mention.uri))
}

/// A message's text taken apart around its markers.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Taken {
    /// What goes to the server: every marker as `@name` where it stood.
    pub sent: String,
    /// The picks in `picked` it names.
    pub mentions: Vec<ResourceMention>,
    /// The `@name` of each marker of a pick not in `picked`.
    pub unknown: Vec<String>,
    /// The draft with those unknown markers as plain `@name` text, the known
    /// ones still chips.
    pub back: String,
    /// Where the first unknown marker's `@name` ends in `back`, and the `@`
    /// list row that picks it again.
    pub again: Option<(usize, String)>,
}

/// `text` taken apart around its markers, knowing the picks in `picked`.
pub(crate) fn take(text: &str, picked: &HashMap<String, ResourceMention>) -> Taken {
    let mut taken = Taken {
        sent: String::new(),
        mentions: Vec::new(),
        unknown: Vec::new(),
        back: String::new(),
        again: None,
    };
    for part in crate::prompt_context::parts(text) {
        let piece = match part {
            crate::prompt_context::Part::Reference { body, .. } => match marked(body) {
                Some(marked) => {
                    let name = at_name(&marked.mention);
                    taken.sent.push_str(&name);
                    if picked.get(&marked.pick) == Some(&marked.mention) {
                        taken.back.push_str(body);
                        if !taken.mentions.contains(&marked.mention) {
                            taken.mentions.push(marked.mention);
                        }
                    } else {
                        taken.back.push_str(&name);
                        taken.again.get_or_insert_with(|| {
                            (
                                taken.back.len(),
                                key(&marked.mention.server, &marked.mention.uri),
                            )
                        });
                        taken.unknown.push(name);
                    }
                    continue;
                }
                None => body,
            },
            crate::prompt_context::Part::Text(text)
            | crate::prompt_context::Part::Paste { body: text, .. } => text,
        };
        taken.sent.push_str(piece);
        taken.back.push_str(piece);
    }
    taken
}

/// Composer text that leaves the TUI without being sent as a prompt (a
/// question's answer, a shell command, a queued message's edit): every
/// marker as plain `@name`, never its JSON.
pub(crate) fn plain(text: &str) -> String {
    take(text, &HashMap::new()).sent
}

/// Where the TUI keeps its picks, beside its history and drafts.
const PICKS: &str = "tui-resource-picks.json";

/// The picks kept for later sessions.
fn kept_picks() -> HashMap<String, ResourceMention> {
    crate::config::load_client_json(PICKS).unwrap_or_default()
}

/// The files that keep prompts across sessions: the history, each server's
/// drafts and the stash.
const SAVED: [&str; 3] = [
    crate::config::HISTORY_FILE,
    crate::servers::DRAFTS_FILE,
    crate::preferences::STASH_FILE,
];

/// Keeps `named` (the picks a prompt just saved names) for later sessions,
/// and forgets the kept picks no saved prompt names any more. The saved
/// prompts are read under the picks' own lock, so a pick another TUI just
/// saved is never forgotten.
fn keep(named: Vec<(String, ResourceMention)>) {
    let _ =
        crate::config::update_client_json(PICKS, |picks: &mut HashMap<String, ResourceMention>| {
            let saved = SAVED.map(crate::config::client_file_text).concat();
            let fresh: HashSet<String> = named.iter().map(|(nonce, _)| nonce.clone()).collect();
            picks.extend(named);
            picks.retain(|nonce, _| fresh.contains(nonce) || saved.contains(nonce.as_str()));
        });
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
        // The rules of the agent the next prompt runs under decide, and
        // while a turn runs that turn's agent's too: asked again when one
        // starts or ends.
        let chat = (
            self.thread.id,
            self.agent().to_owned(),
            self.active_turn.is_some(),
        );
        if fresh && self.mention_resources.chat.as_ref() == Some(&chat) {
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
        self.mention_resources.chat = Some(chat.clone());
        tokio::spawn(list(client.clone(), chat, asked, fresh, sender));
    }

    /// Part of a listing asked off the loop has come. The open `@` list
    /// gains a server's rows below the others, so the row the cursor is on
    /// stays put; a list Esc hid stays hidden. A failed listing is asked for
    /// again at the next `@`.
    pub(crate) fn resources_arrived(&mut self, update: Update) {
        self.list_generation += 1;
        let listing = &mut self.mention_resources;
        match update {
            Update::Servers {
                asked,
                chat,
                turned_off,
            } if listing.asked == Some(asked) && listing.chat.as_ref() == Some(&chat) => {
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
        // A list Esc hid, a shell command or a slash command shows no rows:
        // they wait for the next `@`.
        if self.autocomplete_hidden || self.shell_mode || self.composer.text().starts_with('/') {
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
        self.select_wanted();
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
        // The chip brings its own space: one already after the query (a pick
        // made mid-sentence) is not doubled.
        let mut end = start + 1 + query.len();
        if self.composer.text()[end..].starts_with(' ') {
            end += 1;
        }
        match self
            .composer
            .attach_context(start, end, &label, &marker(&mention, &pick))
        {
            Ok(()) => {
                self.mention_resources.picked.insert(pick, mention);
                self.notice = format!("Attached {label} · Backspace removes the reference");
            }
            Err(error) => self.notice = error.to_string(),
        }
        self.mention_suggestions.clear();
        self.selection = 0;
        self.mention_resources.want = None;
        true
    }

    /// Highlights the row a returned pick names once the open `@` list shows
    /// it, unless the cursor has left the first row meanwhile.
    pub(crate) fn select_wanted(&mut self) {
        let Some(want) = &self.mention_resources.want else {
            return;
        };
        if self.selection != 0 {
            return;
        }
        if let Some(at) = self
            .mention_suggestions
            .iter()
            .position(|suggestion| suggestion == want)
        {
            self.selection = at;
            self.mention_resources.want = None;
        }
    }

    /// A prompt this TUI sends, with its picks as mentions and each as
    /// `@name` where it stood. `None` when a marker names a pick this TUI
    /// does not know (made on another computer, or forgotten): nothing is
    /// sent, the draft comes back with that marker as plain `@name`
    /// (`returned`), and a notice says to pick it again. A server that
    /// cannot read mentions sends nothing either, as in Desktop.
    pub(crate) fn take_mentions(
        &mut self,
        text: &str,
    ) -> Result<Option<(String, Vec<ResourceMention>)>> {
        let mut taken = take(text, &self.mention_resources.picked);
        if !taken.unknown.is_empty() {
            // Picked in an earlier session: kept beside the history.
            self.mention_resources.picked.extend(kept_picks());
            taken = take(text, &self.mention_resources.picked);
        }
        if !taken.unknown.is_empty() {
            self.notice = match taken.unknown.as_slice() {
                [one] => format!("Pick {one} again to include it"),
                many => format!("Pick {} again to include them", many.join(", ")),
            };
            let (at, row) = taken.again.unwrap_or_default();
            self.mention_resources.returned = Some(Returned {
                text: taken.back,
                at,
                row,
            });
            return Ok(None);
        }
        if taken.mentions.is_empty() {
            return Ok(Some((text.to_owned(), Vec::new())));
        }
        if !offered(self) {
            bail!(
                "This server cannot read MCP resources. Remove them from the message, or update the server."
            );
        }
        Ok(Some((taken.sent, taken.mentions)))
    }

    /// Keeps the picks `texts` name for later sessions: called once they are
    /// saved in the history, a draft or the stash, so a chip recalled or
    /// restored after a restart is still read.
    pub(crate) fn keep_picks(&self, texts: &[String]) {
        let named: Vec<(String, ResourceMention)> = self
            .mention_resources
            .picked
            .iter()
            .filter(|(nonce, _)| texts.iter().any(|text| text.contains(nonce.as_str())))
            .map(|(nonce, mention)| (nonce.clone(), mention.clone()))
            .collect();
        if !named.is_empty() {
            keep(named);
        }
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
            directory: None,
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
            default_approval_mode: None,
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
        let taken = take(&marked, &picked);
        // Each reads as `@name` where it stood. The marker whose nonce names
        // another resource is not a pick: it is unknown, and goes back to the
        // prompt as plain text while the pick stays a chip.
        assert_eq!(taken.sent, "Compare @setup with @setup now");
        assert_eq!(taken.mentions, std::slice::from_ref(&setup));
        assert_eq!(taken.unknown, ["@setup"]);
        assert_eq!(
            taken.back,
            format!("Compare {} with @setup now", marker(&setup, "a"))
        );
        // Text no pick of this TUI made (piped to `run`, an editor's over
        // ACP, a schedule's) is no mention.
        assert!(take(&marked, &HashMap::new()).mentions.is_empty());
        // Whatever leaves without being sent as a prompt carries `@name`.
        assert_eq!(plain(&marked), "Compare @setup with @setup now");
        // What the server read for one is text, sent again as it is.
        let read = "Look\n\n<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# Setup\n```\n\n</details>";
        let again = take(read, &picked);
        assert_eq!((again.sent.as_str(), again.mentions.len()), (read, 0));
        assert_eq!(plain(read), read);
        assert!(!is_marker(read) && is_marker(&marker(&setup, "a")));
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
    fn a_pick_recalled_after_a_restart_is_still_read_and_an_unknown_one_comes_back_as_text() {
        let setup = mention("docs", "docs://setup", "setup");
        let mut first = app();
        first.server_info = Some(info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]));
        first.mention_resources.list = vec![resource("docs", "docs://setup", "setup")];
        first.composer.set_text("Explain @se".into());
        assert!(first.attach_resource(&suggestions(&first, "se")[0]));
        let draft = first.composer.take();
        // Sent, the prompt is in the history the next session recalls.
        let scope = format!("restart-{}", uuid::Uuid::now_v7());
        crate::config::save_scoped_history(&scope, "chat", &draft).unwrap();
        first.keep_picks(std::slice::from_ref(&draft));
        // The next session knows the pick from the file kept beside the history.
        let mut next = app();
        next.server_info = Some(info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]));
        let recalled = crate::config::load_scoped_history(&scope, "chat")
            .pop()
            .unwrap();
        assert_eq!(recalled, draft);
        let (text, mentions) = next.take_mentions(&recalled).unwrap().unwrap();
        assert_eq!(
            (text.as_str(), mentions),
            ("Explain @setup ", vec![setup.clone()])
        );
        // A pick neither session made (another computer's, a forgotten one)
        // is not sent: the draft comes back with it as plain text, once.
        let forged = format!("Explain {}", marker(&setup, "not-a-pick"));
        assert!(next.take_mentions(&forged).unwrap().is_none());
        assert_eq!(next.notice, "Pick @setup again to include it");
        let back = next.mention_resources.returned.take().unwrap().text;
        assert_eq!(back, "Explain @setup");
        // Sent again, it is the plain text it now is.
        assert_eq!(
            next.take_mentions(&back).unwrap(),
            Some(("Explain @setup".to_owned(), Vec::new()))
        );
        // A later save forgets the kept picks no saved prompt names.
        let stale = "f".repeat(32);
        let _ = crate::config::update_client_json(
            PICKS,
            |picks: &mut HashMap<String, ResourceMention>| {
                picks.insert(stale.clone(), setup.clone());
            },
        );
        next.mention_resources.list = vec![resource("docs", "docs://setup", "setup")];
        next.composer.set_text("Again @se".into());
        assert!(next.attach_resource(&suggestions(&next, "se")[0]));
        let again = next.composer.take();
        crate::config::save_scoped_history(&scope, "chat", &again).unwrap();
        next.keep_picks(std::slice::from_ref(&again));
        let kept = kept_picks();
        assert!(!kept.contains_key(&stale));
        for prompt in [&draft, &again] {
            assert!(
                kept.keys().any(|nonce| prompt.contains(nonce.as_str())),
                "{prompt}"
            );
        }
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
        app.mention_resources.chat = Some((app.thread.id, app.agent().to_owned(), false));
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
        let (text, mentions) = app.take_mentions(&draft).unwrap().unwrap();
        assert_eq!(text, "Compare @setup ");
        assert_eq!(mentions, [mention("wiki", "docs://setup", "setup")]);
        // History brings the chip back, naming the same pick.
        app.composer.set_text(draft.clone());
        assert_eq!(app.composer.text(), "Compare [@setup] ");
        let recalled = app.composer.expanded_text();
        assert_eq!(app.take_mentions(&recalled).unwrap().unwrap().1.len(), 1);
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
                    ..Default::default()
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
                    ..Default::default()
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
                    ..Default::default()
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

    /// A question's answer is the person's text, not a prompt: a pick in
    /// it leaves as its `@name`, never as the marker's JSON.
    #[tokio::test]
    async fn a_question_answered_with_a_pick_in_the_prompt_answers_with_its_name() {
        let mut app = app();
        let thread = app.thread.id;
        app.reduce(
            haarness_protocol::EventEnvelope {
                schema_version: haarness_protocol::SCHEMA_VERSION.into(),
                server_instance_id: uuid::Uuid::now_v7(),
                event_id: 1,
                event_type: "question.requested".into(),
                thread_id: Some(thread),
                root_thread_id: Some(thread),
                turn_id: None,
                item_id: None,
                payload: serde_json::json!({
                    "questionId": "question-pick",
                    "prompt": "Which page?",
                    "choices": ["first"],
                    "custom": true,
                }),
                created_at: chrono::Utc::now(),
            },
            &ApiClient::new("http://127.0.0.1:9", "token").unwrap(),
        )
        .await
        .unwrap();
        let setup = mention("docs", "docs://setup", "setup");
        app.mention_resources
            .picked
            .insert("a".into(), setup.clone());
        app.composer
            .set_text(format!("Use {} please", marker(&setup, "a")));
        let (url, server) = capture(1).await;
        crate::input::handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &ApiClient::new(&url, "token").unwrap(),
        )
        .await
        .unwrap();
        let bodies = server.await.unwrap();
        assert_eq!(bodies[0]["answer"], "Use @setup please");
    }

    /// A server that answers `/v1/info` with `info`, a file search with a
    /// project file named like the resource, a new turn with a turn, and
    /// anything else with `[]`, until the test ends; with every request it
    /// got, body included.
    async fn answering(
        info: haarness_protocol::ServerInfo,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let info = serde_json::to_string(&info).unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                let complete = |request: &[u8]| {
                    let text = String::from_utf8_lossy(request);
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        return false;
                    };
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    body.len() >= length
                };
                while !complete(&request) {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        break;
                    };
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                log.lock().unwrap().push(text.clone());
                let body = if text.starts_with("GET /v1/info") {
                    info.clone()
                } else if text.starts_with("GET /v1/files/search") {
                    r#"["setup.md"]"#.into()
                } else if text.starts_with("POST /v1/threads/") && text.contains("/turns ") {
                    let now = chrono::Utc::now();
                    serde_json::to_string(&haarness_protocol::Turn {
                        id: haarness_protocol::TurnId::new(),
                        thread_id: haarness_protocol::ThreadId::new(),
                        request_id: haarness_protocol::RequestId::new(),
                        status: haarness_protocol::TurnStatus::Running,
                        agent: "build".into(),
                        provider: "echo".into(),
                        model: "echo-1".into(),
                        ultracode: false,
                        error: None,
                        created_at: now,
                        updated_at: now,
                    })
                    .unwrap()
                } else {
                    "[]".into()
                };
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        (format!("http://{address}"), requests)
    }

    /// A pick this TUI does not know comes back as plain `@setup` with the
    /// cursor after it and its `@` list open on that resource, even below a
    /// project file of the same name, so one Enter picks it again.
    #[tokio::test]
    async fn an_unknown_pick_comes_back_with_its_list_open_to_pick_it_again() {
        let mut app = app();
        app.providers = vec![haarness_protocol::ProviderInfo {
            id: "echo".into(),
            name: "Echo".into(),
            authenticated: true,
            models: Vec::new(),
        }];
        let capable = info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]);
        app.server_info = Some(capable.clone());
        app.mention_resources.list = vec![
            resource("docs", "docs://setup-notes", "setup-notes"),
            resource("docs", "docs://setup", "setup"),
        ];
        app.mention_resources.asked = Some(Instant::now());
        app.mention_resources.chat = Some((app.thread.id, app.agent().to_owned(), false));
        let setup = mention("docs", "docs://setup", "setup");
        app.composer.set_text(format!(
            "Explain {} in two lines",
            marker(&setup, "not-a-pick")
        ));
        let client = ApiClient::new(&answering(capable).await.0, "token").unwrap();
        let enter = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        crate::input::handle_key(&mut app, enter, &client)
            .await
            .unwrap();
        assert_eq!(app.composer.text(), "Explain @setup in two lines");
        assert_eq!(app.composer.cursor(), "Explain @setup".len());
        assert_eq!(app.notice, "Pick @setup again to include it");
        assert_eq!(app.mention_suggestions[0], "setup.md");
        assert_eq!(
            find(&app, &app.mention_suggestions[app.selection]).map(|row| row.uri.as_str()),
            Some("docs://setup")
        );
        // One Enter picks it again, not the file.
        crate::input::handle_key(&mut app, enter, &client)
            .await
            .unwrap();
        assert_eq!(app.composer.text(), "Explain [@setup] in two lines");
        assert!(app.composer.expanded_text().contains("docs://setup\""));
        assert!(app.mention_resources.want.is_none());

        // A list that comes after the draft does highlights the row as it
        // arrives.
        app.mention_resources.list.clear();
        app.composer.set_text("Explain @setup".into());
        app.mention_resources.want = Some(key("docs", "docs://setup"));
        app.mention_suggestions = vec!["setup.md".into()];
        app.selection = 0;
        let asked = app.mention_resources.asked.unwrap();
        app.resources_arrived(Update::Rows {
            asked,
            rows: vec![resource("wiki", "wiki://setup", "setup")],
        });
        assert_eq!(app.selection, 0);
        app.resources_arrived(Update::Rows {
            asked,
            rows: vec![resource("docs", "docs://setup", "setup")],
        });
        assert_eq!(
            find(&app, &app.mention_suggestions[app.selection]).map(|row| row.uri.as_str()),
            Some("docs://setup")
        );

        // Once the person moves in the list, even back to the first row, a
        // late row no longer takes the highlight.
        app.mention_resources.list.clear();
        app.mention_resources.want = Some(key("docs", "docs://setup"));
        app.mention_suggestions = vec!["setup.md".into(), "settings.md".into()];
        app.selection = 0;
        app.move_selection(1);
        app.move_selection(-1);
        assert!(app.mention_resources.want.is_none());
        app.resources_arrived(Update::Rows {
            asked,
            rows: vec![resource("docs", "docs://setup", "setup")],
        });
        assert_eq!(app.selection, 0);
    }

    /// An Esc that hid one prompt's list does not hide the next one's: a
    /// pick that comes back after it opens its list on that resource.
    #[tokio::test]
    async fn a_pick_that_comes_back_after_an_earlier_esc_opens_its_list() {
        let mut app = app();
        app.providers = vec![haarness_protocol::ProviderInfo {
            id: "echo".into(),
            name: "Echo".into(),
            authenticated: true,
            models: Vec::new(),
        }];
        let capable = info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]);
        app.server_info = Some(capable.clone());
        app.mention_resources.list = vec![resource("docs", "docs://setup", "setup")];
        app.mention_resources.asked = Some(Instant::now());
        app.mention_resources.chat = Some((app.thread.id, app.agent().to_owned(), false));
        let (url, _) = answering(capable).await;
        let client = ApiClient::new(&url, "token").unwrap();
        let press =
            |code| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        app.composer.set_text("@set".into());
        app.mention_suggestions = vec!["settings.md".into()];
        crate::input::handle_key(&mut app, press(crossterm::event::KeyCode::Esc), &client)
            .await
            .unwrap();
        crate::input::handle_key(&mut app, press(crossterm::event::KeyCode::Enter), &client)
            .await
            .unwrap();
        assert_eq!(app.composer.text(), "", "Enter sent the text");
        assert!(!app.autocomplete_hidden);
        app.active_turn = None;
        let setup = mention("docs", "docs://setup", "setup");
        app.composer
            .set_text(format!("Explain {}", marker(&setup, "not-a-pick")));
        crate::input::handle_key(&mut app, press(crossterm::event::KeyCode::Enter), &client)
            .await
            .unwrap();
        assert_eq!(app.composer.text(), "Explain @setup");
        assert_eq!(
            app.shown_mentions()
                .get(app.selection)
                .and_then(|key| find(&app, key))
                .map(|row| row.uri.as_str()),
            Some("docs://setup")
        );
    }

    /// A turn starting in the chat asks again which servers it cannot reach
    /// (a message sent then follows that turn's agent too), without listing
    /// the resources again.
    #[tokio::test]
    async fn a_turn_starting_asks_again_which_servers_the_chat_cannot_reach() {
        let mut app = app();
        let capable = info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]);
        app.server_info = Some(capable.clone());
        let (sender, mut updates) = tokio::sync::mpsc::channel(8);
        app.resources_tx = Some(sender);
        let asked = Instant::now();
        app.mention_resources.asked = Some(asked);
        app.mention_resources.chat = Some((app.thread.id, app.agent().to_owned(), false));
        let (url, requests) = answering(capable).await;
        let client = ApiClient::new(&url, "token").unwrap();
        app.ask_resources(&client);
        app.active_turn = Some(haarness_protocol::TurnId::new());
        app.ask_resources(&client);
        let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(&update, Update::Servers { asked: when, chat: (_, _, true), .. } if *when == asked),
            "{update:?}"
        );
        assert_eq!(app.mention_resources.asked, Some(asked));
        let asked_servers = requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("GET /v1/mcp?"))
            .count();
        assert_eq!(asked_servers, 1);
    }

    /// Esc hides the `@` list: a listing that comes afterwards does not fill
    /// it again, so Enter sends the text as typed and nothing hidden is
    /// picked or browsed. The rows wait for the next `@`.
    #[tokio::test]
    async fn a_listing_after_esc_leaves_the_list_hidden_and_enter_sends_the_text() {
        let mut app = app();
        app.providers = vec![haarness_protocol::ProviderInfo {
            id: "echo".into(),
            name: "Echo".into(),
            authenticated: true,
            models: Vec::new(),
        }];
        let capable = info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]);
        app.server_info = Some(capable.clone());
        let asked = Instant::now();
        app.mention_resources.asked = Some(asked);
        app.mention_resources.stale = true;
        app.mention_resources.chat = Some((app.thread.id, app.agent().to_owned(), false));
        app.mention_resources.want = Some(key("docs", "docs://setup"));
        app.composer.set_text("@set".into());
        app.mention_suggestions = vec!["settings.md".into()];
        let (url, requests) = answering(capable).await;
        let client = ApiClient::new(&url, "token").unwrap();
        let press =
            |code| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        crate::input::handle_key(&mut app, press(crossterm::event::KeyCode::Esc), &client)
            .await
            .unwrap();
        assert!(app.autocomplete_hidden);
        assert!(app.mention_resources.want.is_none());
        app.resources_arrived(Update::Rows {
            asked,
            rows: vec![resource("docs", "docs://setup", "setup")],
        });
        assert!(app.mention_suggestions.is_empty());
        assert_eq!(app.mention_resources.list.len(), 1);
        // Whatever fills it, a hidden list is neither browsed nor picked.
        app.mention_suggestions = vec![key("docs", "docs://setup"), "settings.md".into()];
        app.move_selection(1);
        assert_eq!(app.selection, 0);
        assert!(
            !crate::prompt_context::apply(&mut app, &client)
                .await
                .unwrap()
        );
        app.mention_suggestions.clear();
        crate::input::handle_key(&mut app, press(crossterm::event::KeyCode::Enter), &client)
            .await
            .unwrap();
        assert_eq!(app.composer.text(), "");
        let started = requests
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.starts_with("POST /v1/threads/") && request.contains("/turns "))
            .cloned()
            .expect("Enter sent the text");
        assert!(started.contains(r#""text":"@set""#), "{started}");
        assert!(!started.contains("resources"), "{started}");
    }
}
