//! Server switching replaces the REST/SSE/PTTY client together. Execution stays
//! on the selected server; local drafts use a stable workspace identity; remote scopes retain the full endpoint.
use crate::{
    api::{ApiClient, ServerAuth},
    config,
    state::App,
};
use anyhow::{Context, Result, ensure};
use haarness_protocol::{
    ManagementEntry as Entry, ManagementField as Field, ManagementView as View, ThreadId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Serialize, Deserialize)]
struct Server {
    id: String,
    name: String,
    url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    token: String,
    #[serde(default)]
    auth: ServerAuth,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredDraft")]
pub(crate) struct Draft {
    pub text: String,
    pub attachments: Vec<String>,
    pub shell: bool,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredDraft {
    Current {
        text: String,
        attachments: Vec<String>,
        #[serde(default)]
        shell: bool,
    },
    Legacy((String, Vec<String>)),
}
impl From<StoredDraft> for Draft {
    fn from(value: StoredDraft) -> Self {
        match value {
            StoredDraft::Current {
                text,
                attachments,
                shell,
            } => Self {
                text,
                attachments,
                shell,
            },
            StoredDraft::Legacy((text, attachments)) => Self {
                text,
                attachments,
                shell: false,
            },
        }
    }
}
impl Draft {
    pub(crate) fn from_app(app: &App) -> Self {
        // A queued message open for editing already lives on the server; the
        // draft worth keeping is the one set aside when the edit began.
        if let Some(edit) = &app.queue_edit {
            return Self {
                text: edit.draft.expanded_text(),
                attachments: edit.attachments.clone(),
                shell: edit.shell,
            };
        }
        Self {
            text: app.composer.expanded_text(),
            attachments: app.attachments.clone(),
            shell: app.shell_mode,
        }
    }
    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.attachments.is_empty()
    }
}
#[derive(Default, Serialize, Deserialize)]
struct SavedView {
    thread: Option<ThreadId>,
    drafts: HashMap<ThreadId, Draft>,
    // Read the old server-wide mode only for its actual current draft.
    #[serde(default, skip_serializing)]
    shell: bool,
}
type SavedViews = BTreeMap<String, SavedView>;
pub(crate) struct Connection {
    pub client: ApiClient,
    pub app: App,
}
fn entry(label: impl Into<String>, description: impl Into<String>, request: Value) -> Entry {
    Entry {
        label: label.into(),
        description: description.into(),
        request,
        confirm: None,
    }
}
fn field(key: &str, label: &str, secret: bool) -> Field {
    Field {
        key: key.into(),
        label: label.into(),
        value: String::new(),
        required: matches!(key, "url" | "path"),
        secret,
        choices: vec![],
    }
}

fn draft_scope(client: &ApiClient, info: Option<&haarness_protocol::ServerInfo>) -> String {
    if client.supports_local_workspace_switch()
        && let Some(workspace) = info.and_then(|info| info.workspace.as_deref())
    {
        return format!("local:{workspace}");
    }
    client.server_url()
}

fn saved_view(
    client: &ApiClient,
    info: Option<&haarness_protocol::ServerInfo>,
    threads: &[haarness_protocol::Thread],
) -> Result<SavedView> {
    let mut all: SavedViews = config::load_client_json("tui-server-drafts.json")?;
    let scope = draft_scope(client, info);
    let mut saved = if let Some(saved) = all.remove(&scope) {
        saved
    } else if let Some(saved) = all.remove(&client.server_url()) {
        saved
    } else if client.supports_local_workspace_switch() {
        // Recover pre-migration local drafts even if the daemon already changed
        // ports. Import only IDs that the connected workspace actually owns.
        let known: std::collections::HashSet<_> = threads.iter().map(|thread| thread.id).collect();
        let mut recovered = SavedView::default();
        for (url, mut old) in all {
            let local = url::Url::parse(&url).ok().is_some_and(|url| {
                matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            });
            if !local {
                continue;
            }
            if let Some(id) = old.thread.filter(|id| known.contains(id)) {
                recovered.thread = Some(id);
                if old.shell
                    && let Some(draft) = old.drafts.get_mut(&id)
                {
                    draft.shell = true;
                }
            }
            recovered
                .drafts
                .extend(old.drafts.into_iter().filter(|(id, _)| known.contains(id)));
        }
        recovered
    } else {
        SavedView::default()
    };
    if saved.shell
        && let Some(draft) = saved.thread.and_then(|id| saved.drafts.get_mut(&id))
    {
        draft.shell = true;
    }
    Ok(saved)
}

fn apply_saved_drafts(app: &mut App, saved: SavedView) {
    app.draft_baseline.borrow_mut().clone_from(&saved.drafts);
    app.drafts = saved.drafts;
    app.shell_mode = false;
    if let Some(draft) = app
        .drafts
        .remove(&app.thread.id)
        .filter(|_| !app.thread.is_subagent)
    {
        app.composer.set_text(draft.text);
        app.attachments = draft.attachments;
        app.shell_mode = draft.shell;
    }
}

pub(crate) fn restore_drafts(app: &mut App, client: &ApiClient) -> Result<()> {
    let saved = saved_view(client, app.server_info.as_ref(), &app.threads)?;
    apply_saved_drafts(app, saved);
    Ok(())
}

fn merge_drafts(
    saved: &mut HashMap<ThreadId, Draft>,
    baseline: &HashMap<ThreadId, Draft>,
    current: &HashMap<ThreadId, Draft>,
) {
    // Only write changes made by this client. Another window may have saved
    // unrelated drafts since we loaded our snapshot, including deletions.
    for id in baseline.keys().chain(current.keys()) {
        if current.get(id) == baseline.get(id) {
            continue;
        }
        if let Some(draft) = current.get(id) {
            saved.insert(*id, draft.clone());
        } else {
            saved.remove(id);
        }
    }
}

pub(crate) fn save_current(app: &App, client: &ApiClient) -> Result<()> {
    let scope = draft_scope(client, app.server_info.as_ref());
    let mut drafts = app.drafts.clone();
    let draft = Draft::from_app(app);
    if draft.is_empty() {
        drafts.remove(&app.thread.id);
    } else {
        drafts.insert(app.thread.id, draft);
    }
    config::update_client_json("tui-server-drafts.json", |all: &mut SavedViews| {
        let saved = all.entry(scope).or_insert_with(|| SavedView {
            drafts: app.draft_baseline.borrow().clone(),
            ..SavedView::default()
        });
        merge_drafts(&mut saved.drafts, &app.draft_baseline.borrow(), &drafts);
        saved.thread = Some(app.thread.id);
        saved.shell = false;
    })?;
    *app.draft_baseline.borrow_mut() = drafts;
    Ok(())
}
impl App {
    pub(crate) async fn servers_view(
        &mut self,
        input: &Value,
        current: &ApiClient,
    ) -> Result<View> {
        let mut servers: Vec<Server> = config::load_client_json("tui-servers.json")?;
        let action = input["action"].as_str().unwrap_or("view");
        if action == "form" {
            return Ok(View {
                title: "Add server".into(),
                fields: vec![
                    field("url", "Server URL", false),
                    field("name", "Name (optional)", false),
                    field("username", "Username (optional)", false),
                    field("password", "Password (optional)", true),
                ],
                submit: Some(json!({"action":"add"})),
                ..View::default()
            });
        }
        if action == "add" {
            let url = input["url"]
                .as_str()
                .context("Enter server URL")?
                .trim()
                .trim_end_matches('/');
            let parsed = url::Url::parse(url)?;
            ensure!(
                parsed.query().is_none() && parsed.fragment().is_none(),
                "Server URLs cannot contain a query or fragment"
            );
            let name = input["name"].as_str().unwrap_or_default().trim();
            let name = if name.is_empty() {
                parsed.host_str().unwrap_or(url)
            } else {
                name
            };
            let auth = ServerAuth::basic(
                input["username"].as_str().unwrap_or_default(),
                input["password"].as_str().unwrap_or_default(),
            )?;
            ensure!(
                !servers.iter().any(|s| s.url == url),
                "This server is already saved"
            );
            ApiClient::with_auth(url, auth.clone())?.info().await?;
            servers.push(Server {
                id: uuid::Uuid::now_v7().to_string(),
                name: name.into(),
                url: url.into(),
                token: String::new(),
                auth,
            });
            config::save_client_json("tui-servers.json", &servers)?;
        }
        if action == "remove" {
            let id = input["id"].as_str().context("Missing server")?;
            servers.retain(|server| server.id != id);
            config::save_client_json("tui-servers.json", &servers)?;
        }
        if action == "inspect" {
            let server = servers
                .iter()
                .find(|server| input["id"] == server.id)
                .context("Server no longer exists")?;
            return Ok(View {
                title: server.name.clone(),
                lines: vec![server.url.clone()],
                entries: vec![
                    entry(
                        "Connect",
                        "Keep work running on the current server",
                        json!({"action":"connect","id":server.id}),
                    ),
                    Entry {
                        confirm: Some("Forget this saved connection?".into()),
                        ..entry(
                            "Forget server",
                            "Does not stop the server or delete its sessions",
                            json!({"action":"remove","id":server.id}),
                        )
                    },
                ],
                ..View::default()
            });
        }
        if action == "folder_form" {
            return Ok(View {
                title: "Open server folder".into(),
                fields: vec![field("path", "Absolute folder path on this server", false)],
                submit: Some(json!({"action":"open_folder"})),
                ..View::default()
            });
        }
        if action == "projects" {
            let host = current.host_client()?;
            let catalog = host.host_projects().await?;
            let mut view = View {
                title: "Server projects".into(),
                entries: vec![entry(
                    "Open another folder",
                    "Use a path on this server",
                    json!({"action":"folder_form"}),
                )],
                ..View::default()
            };
            for workspace in catalog["workspaces"].as_array().into_iter().flatten() {
                view.entries.push(entry(
                    workspace["name"].as_str().unwrap_or("Workspace"),
                    workspace["path"].as_str().unwrap_or(""),
                    json!({"action":"project","id":workspace["id"]}),
                ));
            }
            return Ok(view);
        }
        if matches!(action, "connect" | "original" | "project" | "open_folder") {
            ensure!(
                self.workspace_mutation.is_none() && self.workspace_ref_operation.is_none(),
                "Wait for the current folder operation to finish"
            );
            let target = if action == "original" {
                self.original_client
                    .clone()
                    .unwrap_or_else(|| current.clone())
            } else if action == "project" || action == "open_folder" {
                let host = current.host_client()?;
                let id = if action == "open_folder" {
                    let result = host
                        .host_open(input["path"].as_str().context("Enter a folder path")?)
                        .await?;
                    result["id"]
                        .as_str()
                        .context("Server did not return a folder")?
                        .to_owned()
                } else {
                    input["id"].as_str().context("Select a project")?.into()
                };
                ensure!(
                    id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                    "Invalid workspace ID"
                );
                let credentials = host.credentials();
                ApiClient::with_auth(
                    &format!("{}/v1/host/workspaces/{id}", credentials.server),
                    credentials.auth,
                )?
                .with_attachment_uploads()
            } else {
                let server = servers
                    .iter()
                    .find(|server| input["id"] == server.id)
                    .context("Server no longer exists")?;
                ApiClient::with_auth(
                    &server.url,
                    if server.auth == ServerAuth::None {
                        ServerAuth::bearer(&server.token)
                    } else {
                        server.auth.clone()
                    },
                )?
                .with_attachment_uploads()
            };
            save_current(self, current)?;
            let connection = prepare(target, self).await?;
            self.pending_server = Some(Box::new(connection));
            return Ok(View {
                title: "Connecting…".into(),
                ..View::default()
            });
        }
        let mut view = View {
            title: "Servers".into(),
            lines: vec![format!("Current connection: {}", current.server_url())],
            entries: vec![
                entry(
                    "Original server",
                    "Return to the server used at startup",
                    json!({"action":"original"}),
                ),
                entry(
                    "Projects on this server",
                    "Select a folder without stopping other work",
                    json!({"action":"projects"}),
                ),
                entry(
                    "Add server",
                    "Save a server connection",
                    json!({"action":"form"}),
                ),
            ],
            ..View::default()
        };
        for server in servers {
            view.entries.push(entry(
                server.name,
                server.url,
                json!({"action":"inspect","id":server.id}),
            ));
        }
        Ok(view)
    }
}
async fn prepare(client: ApiClient, previous: &App) -> Result<Connection> {
    let info = client.info().await?;
    ensure!(
        info.schema_version == haarness_protocol::SCHEMA_VERSION,
        "Server schema is incompatible; update this server"
    );
    let (providers, mut threads, agents) =
        tokio::try_join!(client.providers(), client.threads(), client.agents())?;
    let saved = saved_view(&client, Some(&info), &threads)?;
    let thread = if let Some(thread) = threads
        .iter()
        .find(|t| Some(t.id) == saved.thread)
        .or_else(|| threads.first())
    {
        thread.clone()
    } else {
        let thread = client
            .create_thread(haarness_protocol::CreateThreadRequest::default())
            .await?;
        threads.push(thread.clone());
        thread
    };
    let items = client.items(thread.id).await?;
    let mut app = App::new(thread, threads, items, providers, agents);
    app.original_client = previous.original_client.clone();
    app.workspace_local_switching = client.supports_local_workspace_switch();
    app.server_info = Some(info);
    app.server_commands = client.commands().await?;
    app.mcp_status = client.mcp().await?;
    if let Ok(catalog) = client.workflows().await {
        app.workflow_definitions = catalog.workflows;
        app.workflow_invalid = catalog.invalid;
    }
    app.hydrate_active_turns(client.active_turns().await?);
    app.hydrate_questions(client.pending_questions().await?);
    app.hydrate_permissions(client.pending_permissions().await?, &client)
        .await;
    crate::hydrate_initial_goal(&mut app, &client).await;
    if let Ok(todos) = client.todos(app.thread.id).await {
        app.todos = todos;
    }
    app.sync_prompt_history(&client);
    apply_saved_drafts(&mut app, saved);
    app.themes = config::load_theme_entries();
    app.theme_selected = previous.theme_selected;
    app.apply_theme(&previous.theme_name, &previous.theme_variant);
    app.language = previous.language;
    app.notice = "Connected to selected server".into();
    Ok(Connection { client, app })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str, shell: bool) -> Draft {
        Draft {
            text: text.into(),
            attachments: vec![],
            shell,
        }
    }

    #[test]
    fn separate_clients_merge_changes_without_losing_or_resurrecting_other_drafts() {
        let a = ThreadId::new();
        let b = ThreadId::new();
        let original = HashMap::from([(a, draft("original A", false))]);
        let mut disk = original.clone();
        let first = HashMap::from([(a, draft("edited A", true))]);
        merge_drafts(&mut disk, &original, &first);
        let second = HashMap::from([(a, draft("original A", false)), (b, draft("B", false))]);
        merge_drafts(&mut disk, &original, &second);
        assert_eq!(disk[&a], draft("edited A", true));
        assert_eq!(disk[&b], draft("B", false));
        merge_drafts(&mut disk, &first, &HashMap::new());
        assert!(!disk.contains_key(&a));
        let third = HashMap::from([(a, draft("original A", false)), (b, draft("new B", false))]);
        merge_drafts(&mut disk, &second, &third);
        assert!(
            !disk.contains_key(&a),
            "an untouched stale draft must not be resurrected"
        );
        assert_eq!(disk[&b].text, "new B");
    }

    #[test]
    fn old_tuple_drafts_and_new_per_draft_modes_deserialize() {
        let old: Draft = serde_json::from_value(json!(["old text", ["image.png"]])).unwrap();
        assert_eq!(old.text, "old text");
        assert_eq!(old.attachments, ["image.png"]);
        assert!(!old.shell);
        let new = draft("printf test", true);
        assert_eq!(
            serde_json::from_value::<Draft>(serde_json::to_value(&new).unwrap()).unwrap(),
            new
        );
    }

    #[test]
    fn restoring_a_draft_uses_its_mode_and_resets_an_unrelated_shell_mode() {
        let thread = haarness_protocol::Thread {
            id: ThreadId::new(),
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
            schedule: None,
        };
        let id = thread.id;
        let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
        app.shell_mode = true;
        apply_saved_drafts(
            &mut app,
            SavedView {
                thread: Some(id),
                drafts: HashMap::from([(id, draft("normal chat", false))]),
                shell: false,
            },
        );
        assert!(!app.shell_mode);
        assert_eq!(app.composer.text(), "normal chat");
        apply_saved_drafts(
            &mut app,
            SavedView {
                thread: Some(id),
                drafts: HashMap::from([(id, draft("printf test", true))]),
                shell: false,
            },
        );
        assert!(app.shell_mode);
        apply_saved_drafts(&mut app, SavedView::default());
        assert!(!app.shell_mode);
    }
}
