//! The `@` list's file search, off the key path. Each edit of an `@` query
//! asks the server again and abandons the search before it; an answer is
//! shown when it comes, and only while the prompt still holds the query it
//! answers. A search that is slow or never answers (a path on a network
//! mount that went away, a server far off) never holds up typing: the list
//! shows the last answer until a newer one comes.
use crate::{api::ApiClient, state::App};
use std::time::Duration;
use tokio::task::JoinHandle;

/// How long Tab or Enter waits for the answer to what was just typed before
/// picking from the list as it stands.
const SETTLE: Duration = Duration::from_millis(500);

/// The search on its way, if any.
#[derive(Default)]
pub(crate) struct FileSearch {
    /// The `@` query asked about, and the task asking.
    pending: Option<(String, JoinHandle<Vec<String>>)>,
    /// Set by the event loop, which shows each answer as it comes (see
    /// [`App::files_found`]). Without it, a search is waited for in place.
    pub(crate) background: bool,
}

impl FileSearch {
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Abandons the search on its way.
    pub(crate) fn stop(&mut self) {
        if let Some((_, task)) = self.pending.take() {
            task.abort();
        }
    }

    /// The query and files of the search on its way, once it answers;
    /// never while none is on its way. Dropped before then, the search goes
    /// on.
    pub(crate) async fn answer(&mut self) -> (String, Vec<String>) {
        let Some((_, task)) = &mut self.pending else {
            return std::future::pending().await;
        };
        let files = task.await.unwrap_or_default();
        let query = self
            .pending
            .take()
            .map(|(query, _)| query)
            .unwrap_or_default();
        (query, files)
    }
}

/// Asks the server for the files that complete the `@` query `query`: off
/// the key path when the event loop shows the answers, in place otherwise.
pub(crate) async fn search(app: &mut App, client: &ApiClient, query: String) {
    let (path, _) = crate::prompt_context::range(&query);
    let asking = {
        let client = client.clone();
        let path = path.to_owned();
        async move { client.search_files(&path, 8).await.unwrap_or_default() }
    };
    app.file_search.stop();
    if app.file_search.background {
        app.selection = 0;
        app.file_search.pending = Some((query, tokio::spawn(asking)));
    } else {
        let files = asking.await;
        app.show_files(&query, files);
    }
}

/// Before Tab or Enter picks from the `@` list (see
/// `crate::prompt_context::apply`), the answer to what was just typed gets a
/// moment to come, so a quick pick takes from it as it would have from a
/// list that came at once. A search that does not answer in time leaves the
/// list as it stands.
pub(crate) async fn settle(app: &mut App) {
    if !app.file_search.is_pending() {
        return;
    }
    if let Ok((query, files)) = tokio::time::timeout(SETTLE, app.file_search.answer()).await {
        app.files_found(&query, files);
    }
}

impl App {
    /// The `@` list for `query` with `files`, the server's answer: the
    /// agents that match, then the files, then the MCP resources.
    pub(crate) fn show_files(&mut self, query: &str, files: Vec<String>) {
        let (_, range) = crate::prompt_context::range(query);
        let mut suggestions = self
            .agents
            .iter()
            .filter(|a| {
                !a.hidden
                    && matches!(a.mode.as_deref(), Some("subagent" | "all"))
                    && a.id
                        .to_ascii_lowercase()
                        .contains(&query.to_ascii_lowercase())
            })
            .take(8)
            .map(|a| format!("agent://{}", a.id))
            .collect::<Vec<_>>();
        suggestions.extend(files.into_iter().map(|file| {
            if let Some((start, end)) = range {
                format!("{file}:{start}-{end}")
            } else {
                file
            }
        }));
        // MCP resources follow agents and files, so a server with many
        // never pushes files down.
        suggestions.extend(crate::resource_mentions::suggestions(self, query));
        self.mention_suggestions = suggestions;
        self.list_generation += 1;
        self.selection = 0;
        self.select_wanted();
    }

    /// A search off the key path answered `query` with `files`: shown while
    /// the prompt still holds that query. A row the person moved to since
    /// stays highlighted where the new list puts it.
    pub(crate) fn files_found(&mut self, query: &str, files: Vec<String>) {
        if self.shell_mode || self.composer.text().starts_with('/') {
            return;
        }
        if self
            .composer
            .mention_query()
            .is_none_or(|(_, current)| current != query)
        {
            return;
        }
        let held = (self.selection != 0)
            .then(|| self.mention_suggestions.get(self.selection).cloned())
            .flatten();
        self.show_files(query, files);
        if let Some(at) = held.and_then(|row| {
            self.mention_suggestions
                .iter()
                .position(|suggestion| *suggestion == row)
        }) {
            self.selection = at;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{api::ApiClient, state::App};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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
        let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
        app.file_search.background = true;
        app
    }

    /// A server whose file search answers a query with `<query>.md` at
    /// once, `./docs/` after a moment for a query starting with `./d`, and
    /// never for a query holding `slow`; anything else with `[]`.
    async fn searching() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 4096];
                    while !request.windows(4).any(|end| end == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let query = text
                        .strip_prefix("GET /v1/files/search?q=")
                        .and_then(|rest| rest.split('&').next());
                    let body = match query {
                        Some(query) if query.contains("slow") => {
                            std::future::pending::<()>().await;
                            unreachable!()
                        }
                        Some(query) if query.starts_with("./d") => {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            r#"["./docs/"]"#.to_owned()
                        }
                        Some(query) => format!(r#"["{query}.md"]"#),
                        None => "[]".to_owned(),
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
                });
            }
        });
        format!("http://{address}")
    }

    /// Presses `code`, which must be handled within a second.
    async fn press(app: &mut App, client: &ApiClient, code: KeyCode) {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        tokio::time::timeout(
            Duration::from_secs(1),
            crate::input::handle_key(app, key, client),
        )
        .await
        .expect("a key waited on the file search")
        .unwrap();
    }

    async fn answered(app: &mut App) {
        let (query, files) = tokio::time::timeout(Duration::from_secs(5), app.file_search.answer())
            .await
            .expect("the file search did not answer");
        app.files_found(&query, files);
    }

    #[tokio::test]
    async fn typing_goes_on_while_a_file_search_does_not_answer() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        for character in "@slow".chars() {
            press(&mut app, &client, KeyCode::Char(character)).await;
        }
        assert_eq!(app.composer.text(), "@slow");
        assert!(app.file_search.is_pending());
        // Typing on abandons the search that hangs; the next one answers.
        for _ in 0..3 {
            press(&mut app, &client, KeyCode::Backspace).await;
        }
        assert_eq!(app.composer.text(), "@s");
        answered(&mut app).await;
        assert_eq!(app.mention_suggestions, ["s.md"]);
        assert!(!app.file_search.is_pending());
    }

    #[tokio::test]
    async fn tab_waits_a_moment_for_the_answer_to_what_was_just_typed() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        for character in "@./do".chars() {
            press(&mut app, &client, KeyCode::Char(character)).await;
        }
        assert!(app.mention_suggestions.is_empty());
        press(&mut app, &client, KeyCode::Tab).await;
        assert_eq!(app.composer.text(), "@./docs/");
    }

    #[test]
    fn an_answer_shows_only_while_the_prompt_holds_its_query() {
        let mut app = app();
        app.composer.set_text("@new".into());
        app.files_found("old", vec!["old.md".into()]);
        assert!(app.mention_suggestions.is_empty());
        app.files_found("new", vec!["new.md".into(), "news.md".into()]);
        assert_eq!(app.mention_suggestions, ["new.md", "news.md"]);
        assert_eq!(app.selection, 0);

        // A row the person moved to stays highlighted where the new list
        // puts it.
        app.move_selection(1);
        app.files_found("new", vec!["renewed.md".into(), "news.md".into()]);
        assert_eq!(app.mention_suggestions[app.selection], "news.md");

        // Nothing shows once the query has gone.
        app.composer.set_text("plain".into());
        app.files_found("new", vec!["later.md".into()]);
        assert_eq!(app.mention_suggestions, ["renewed.md", "news.md"]);
    }

    /// A picked row is what the server's answer says it is: nothing here
    /// looks for it on this computer's disk, where it need not be.
    #[test]
    fn a_picked_row_is_what_the_server_says_it_is() {
        let mut app = app();
        let mut info = haarness_protocol::ServerInfo {
            name: "haarness".into(),
            version: "0.1.0".into(),
            schema_version: "1".into(),
            server_instance_id: uuid::Uuid::now_v7(),
            event_cursor: 0,
            capabilities: Vec::new(),
            workspace: None,
            git_branch: None,
            default_provider: None,
            default_model: None,
            default_approval_mode: None,
            default_reasoning_effort: None,
            default_agent: None,
            project_trusted: None,
            pid: None,
        };
        let workspace =
            std::env::temp_dir().join(format!("haarness-absent-{}", uuid::Uuid::now_v7()));
        info.workspace = Some(workspace.display().to_string());
        app.server_info = Some(info);
        let local = ApiClient::new("http://127.0.0.1:1", "token").unwrap();
        let attached = ApiClient::new("http://127.0.0.1:1", "token")
            .unwrap()
            .with_attachment_uploads();
        let rows = vec!["../far/".to_owned(), "../far/photo.png".to_owned()];
        let pick = |app: &mut App, row: usize, client: &ApiClient| {
            app.composer.set_text("@../f".into());
            app.mention_suggestions.clone_from(&rows);
            app.selection = row;
            assert!(crate::input::apply_mention(app, client));
        };

        // A folder that is on no disk here still opens its next level.
        pick(&mut app, 0, &local);
        assert_eq!(app.composer.text(), "@../far/");

        // A media file the server reads itself becomes an attachment.
        pick(&mut app, 1, &local);
        assert_eq!(app.composer.text(), "");
        assert_eq!(
            app.attachments,
            [workspace.join("../far/photo.png").display().to_string()]
        );
        assert_eq!(app.notice, "Attached ../far/photo.png");

        // A client that sends its attachments from this computer keeps it
        // as text: the file is on the server's.
        app.attachments.clear();
        pick(&mut app, 1, &attached);
        assert_eq!(app.composer.text(), "@../far/photo.png ");
        assert!(app.attachments.is_empty());
    }
}
