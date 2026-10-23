//! MCP resources in the `@` list. The attached server's MCP servers are
//! asked for theirs the first time the list opens, off the event loop, and
//! the answer is kept for a minute; never at startup. A picked resource is a
//! chip whose draft text is a marker naming it, so drafts, history and undo
//! keep it like any other reference; the client takes the markers out when
//! the message is sent and sends them as the request's `resources`, which
//! the server reads into the message (Desktop sends the same field).
use crate::{api::ApiClient, state::App};
use haarness_protocol::{RESOURCE_MENTIONS_CAPABILITY, ResourceMention};
use std::time::{Duration, Instant};

/// The most resource rows the `@` list shows, after agents and files.
pub(crate) const ROWS: usize = 50;
const TTL: Duration = Duration::from_secs(60);
/// A resource's suggestion: this, then its server and URI.
const KEY: &str = "mcp-resource\u{1f}";
const SUMMARY: &str = "<details>\n<summary>MCP resource: ";

/// One resource the `@` list can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resource {
    pub server: String,
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
}

/// The attached server's resources, and when they were asked for. Asked and
/// not yet answered is an `asked` time with the earlier list.
#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub list: Vec<Resource>,
    pub asked: Option<Instant>,
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

/// Every connected server's resources, each asked at once; one that fails
/// lists nothing. `None` when the servers could not be listed.
async fn list(client: &ApiClient) -> Option<Vec<Resource>> {
    let servers = client.mcp().await.ok()?;
    let lists = futures::future::join_all(
        servers
            .iter()
            .filter(|server| server.status == "connected")
            .map(|server| async move {
                client
                    .mcp_resources(&server.name)
                    .await
                    .map(|listed| rows(&server.name, &listed))
                    .unwrap_or_default()
            }),
    )
    .await;
    Some(lists.concat())
}

/// The resource rows for what follows `@`: matched by name only, names that
/// start with it first, at most [`ROWS`].
pub(crate) fn suggestions(app: &App, query: &str) -> Vec<String> {
    if !offered(app) {
        return Vec::new();
    }
    let query = query.to_lowercase();
    let (mut starts, mut holds) = (Vec::new(), Vec::new());
    for resource in &app.mention_resources.list {
        let name = resource.name.to_lowercase();
        if name.starts_with(&query) {
            starts.push(resource);
        } else if name.contains(&query) {
            holds.push(resource);
        }
    }
    starts
        .into_iter()
        .chain(holds)
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

/// The draft text of a picked resource: a block under the label the sent
/// one will have, holding the mention itself.
fn marker(resource: &Resource) -> String {
    let mention = ResourceMention {
        server: resource.server.clone(),
        uri: resource.uri.clone(),
        name: Some(resource.name.clone()),
    };
    let address = format!("{}:{}", resource.server, resource.uri);
    let label = if resource.name == resource.uri {
        address
    } else {
        format!("{} · {address}", resource.name)
    };
    let label = label
        .replace(['\n', '\r'], " ")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!(
        "{SUMMARY}{label}</summary>\n\n{}\n\n</details>",
        serde_json::to_string(&mention).unwrap_or_default()
    )
}

/// The mention a reference's text is, when it is a picked resource's marker
/// rather than what the server read for one.
fn mention(body: &str) -> Option<ResourceMention> {
    let inner = body
        .strip_prefix(SUMMARY)?
        .split_once("</summary>\n\n")?
        .1
        .strip_suffix("\n\n</details>")?;
    serde_json::from_str(inner).ok()
}

/// A message's text without its resource markers, and the mentions they
/// named. A chip's trailing space goes with it, so the words around it read
/// as typed.
pub(crate) fn take(text: &str) -> (String, Vec<ResourceMention>) {
    let mut kept = String::new();
    let mut mentions = Vec::new();
    let mut joined = false;
    for part in crate::prompt_context::parts(text) {
        let piece = match part {
            crate::prompt_context::Part::Reference { body, .. } => {
                if let Some(mention) = mention(body) {
                    if !mentions.contains(&mention) {
                        mentions.push(mention);
                    }
                    joined = kept.is_empty() || kept.ends_with(char::is_whitespace);
                    continue;
                }
                body
            }
            crate::prompt_context::Part::Text(text)
            | crate::prompt_context::Part::Paste { body: text, .. } => text,
        };
        kept.push_str(if joined {
            piece.strip_prefix(' ').unwrap_or(piece)
        } else {
            piece
        });
        joined = false;
    }
    if mentions.is_empty() {
        return (text.to_owned(), mentions);
    }
    (kept.trim_end().to_owned(), mentions)
}

impl App {
    /// Asks for the resources when the `@` list opens and no answer younger
    /// than a minute is kept or on its way.
    pub(crate) fn ask_resources(&mut self, client: &ApiClient) {
        if !offered(self)
            || self
                .mention_resources
                .asked
                .is_some_and(|asked| asked.elapsed() < TTL)
        {
            return;
        }
        let Some(sender) = self.resources_tx.clone() else {
            return;
        };
        self.mention_resources.asked = Some(Instant::now());
        let client = client.clone();
        tokio::spawn(async move {
            let _ = sender.send(list(&client).await).await;
        });
    }

    /// A listing asked off the loop has come: the open `@` list gains its
    /// rows after the others, keeping the row the cursor is on. A failed one
    /// is asked for again at the next `@`.
    pub(crate) fn resources_arrived(&mut self, listed: Option<Vec<Resource>>) {
        match listed {
            Some(list) => self.mention_resources.list = list,
            None => self.mention_resources.asked = None,
        }
        if self.shell_mode || self.composer.text().starts_with('/') {
            return;
        }
        let Some((_, query)) = self.composer.mention_query() else {
            return;
        };
        self.mention_suggestions
            .retain(|suggestion| !suggestion.starts_with(KEY));
        let rows = suggestions(self, &query);
        self.mention_suggestions.extend(rows);
    }

    /// Picks the resource row `suggestion` names: it becomes a chip in
    /// place of the `@` query. False when it names no resource.
    pub(crate) fn attach_resource(&mut self, suggestion: &str) -> bool {
        let Some(resource) = find(self, suggestion).cloned() else {
            return false;
        };
        let Some((start, query)) = self.composer.mention_query() else {
            return false;
        };
        let label = format!("@{}", resource.name);
        match self.composer.attach_context(
            start,
            start + 1 + query.len(),
            &label,
            &marker(&resource),
        ) {
            Ok(()) => self.notice = format!("Attached {label} · Backspace removes the reference"),
            Err(error) => self.notice = error.to_string(),
        }
        self.mention_suggestions.clear();
        self.selection = 0;
        true
    }
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

    #[test]
    fn a_marker_reads_as_its_chip_and_is_sent_as_a_mention() {
        let setup = resource("docs", "docs://setup", "setup");
        let marked = format!("Compare {} and {} now ", marker(&setup), marker(&setup));
        // Shown and recalled as the chip it was picked as.
        assert_eq!(
            crate::prompt_context::display(&marked),
            "Compare @setup and @setup now "
        );
        let (text, mentions) = take(&marked);
        assert_eq!(text, "Compare and now");
        assert_eq!(
            mentions,
            [ResourceMention {
                server: "docs".into(),
                uri: "docs://setup".into(),
                name: Some("setup".into()),
            }]
        );
        // What the server read for one is text, sent again as it is.
        let read = "Look\n\n<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# Setup\n```\n\n</details>";
        assert_eq!(take(read), (read.to_owned(), Vec::new()));
        assert_eq!(crate::prompt_context::display(read), "Look\n\n@setup");
        let unread = "<details>\n<summary>MCP resource: wiki:docs://&lt;x&gt; (not read)</summary>\n\n```\nCould not read it\n```\n\n</details>";
        assert_eq!(
            crate::prompt_context::display(unread),
            "@wiki:docs://<x> (not read)"
        );
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
    fn rows_match_by_name_starting_ones_first_and_stop_at_fifty() {
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
        assert_eq!(names(&app, "UP"), ["upload", "setup"]);
        assert_eq!(names(&app, "tables"), ["tables"]);
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
    fn a_listing_joins_the_open_list_and_a_pick_is_a_chip_sent_as_a_mention() {
        let mut app = app();
        app.server_info = Some(info(vec![RESOURCE_MENTIONS_CAPABILITY.into()]));
        app.composer.set_text("Compare @se".into());
        app.mention_suggestions = vec!["src/setup.rs".into()];
        app.resources_arrived(Some(vec![
            resource("docs", "docs://setup", "setup"),
            resource("wiki", "docs://setup", "setup"),
            resource("docs", "docs://logo", "logo"),
        ]));
        // After the files, the matching ones, each server's its own row.
        let listed = app.mention_suggestions.clone();
        assert_eq!(listed.len(), 3, "{listed:?}");
        assert_eq!(listed[0], "src/setup.rs");
        assert_eq!(find(&app, &listed[2]).unwrap().server, "wiki");
        assert!(app.attach_resource(&listed[2]));
        assert_eq!(app.composer.text(), "Compare [@setup] ");
        assert!(app.notice.starts_with("Attached @setup"), "{}", app.notice);
        let sent = app.composer.take();
        let (text, mentions) = take(&sent);
        assert_eq!(text, "Compare");
        assert_eq!(
            mentions,
            [ResourceMention {
                server: "wiki".into(),
                uri: "docs://setup".into(),
                name: Some("setup".into()),
            }]
        );
        // History brings the chip back, naming the same resource.
        app.composer.set_text(sent.clone());
        assert_eq!(app.composer.text(), "Compare [@setup] ");
        assert_eq!(app.composer.expanded_text(), sent);
        // A listing that failed is asked for again at the next `@`.
        app.mention_resources.asked = Some(Instant::now());
        app.resources_arrived(None);
        assert!(app.mention_resources.asked.is_none());
    }
}
