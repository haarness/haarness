//! The `@` list's file search, off the key path. Each edit of an `@` query
//! asks the server again and abandons the search before it; an answer is
//! shown when it comes, and only while the prompt still holds the query it
//! answers. A search that is slow or never answers (a folder on an offline
//! network volume, a server far off or busy) never holds up typing: the list
//! shows the last answer until a newer one comes.
//!
//! Tab and Enter pick only from rows that answer the query typed. Pressed
//! while that answer is on its way, the key is held, and handled as soon as
//! the answer is shown, as if it had come at once; Esc or any other key lets
//! it go. Up, Down and Esc treat the list as open meanwhile.
use crate::{api::ApiClient, state::App};
use crossterm::event::KeyEvent;
use tokio::task::JoinHandle;

/// The search on its way, if any, and what the list shows.
#[derive(Default)]
pub(crate) struct FileSearch {
    /// The `@` query asked about, and the task asking.
    pending: Option<(String, JoinHandle<Vec<String>>)>,
    /// The `@` query the rows of the list answer.
    shown: Option<String>,
    /// A Tab or Enter pressed while the answer to the query typed was on its
    /// way, handled once it is shown.
    held: Option<KeyEvent>,
}

impl FileSearch {
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Abandons the search on its way, and the key held for it.
    pub(crate) fn stop(&mut self) {
        self.held = None;
        if let Some((_, task)) = self.pending.take() {
            task.abort();
        }
    }

    /// Lets go of a held Tab or Enter.
    pub(crate) fn release(&mut self) {
        self.held = None;
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

/// Asks the server, off the key path, for the files that complete the `@`
/// query `query`; the event loop shows the answer (see [`App::files_found`]).
pub(crate) fn search(app: &mut App, client: &ApiClient, query: String) {
    let (path, _) = crate::prompt_context::range(&query);
    let asking = {
        let client = client.clone();
        let path = path.to_owned();
        async move { client.search_files(&path, 8).await.unwrap_or_default() }
    };
    app.file_search.stop();
    app.selection = 0;
    app.file_search.pending = Some((query, tokio::spawn(asking)));
}

/// Whether Tab or Enter (`key`) waits to pick from the `@` list: while the
/// answer to the query typed is on its way, or when the rows answer another
/// query (the query typed is then asked about). The key is then held and
/// handled once that answer is shown; it never falls through to switching
/// the agent or sending the message meanwhile.
pub(crate) fn hold(app: &mut App, client: &ApiClient, key: KeyEvent) -> bool {
    if app.autocomplete_hidden || app.shell_mode || app.composer.text().starts_with('/') {
        return false;
    }
    let Some((_, query)) = app.composer.mention_query() else {
        return false;
    };
    let asked = app
        .file_search
        .pending
        .as_ref()
        .is_some_and(|(asked, _)| *asked == query);
    if !asked {
        if app.file_search.shown.as_deref() == Some(query.as_str()) {
            return false;
        }
        search(app, client, query);
    }
    app.file_search.held = Some(key);
    true
}

impl App {
    /// Whether the `@` list counts as open: it shows rows, or the files of
    /// the query typed are on their way to it.
    pub(crate) fn mention_list_open(&self) -> bool {
        !self.shown_mentions().is_empty()
            || (self.file_search.is_pending() && !self.autocomplete_hidden)
    }

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
        self.file_search.shown = Some(query.to_owned());
        self.list_generation += 1;
        self.selection = 0;
        self.select_wanted();
    }

    /// A search off the key path answered `query` with `files`: shown while
    /// the prompt still holds that query, when the Tab or Enter held for it
    /// is handed back, to be handled now. A row the person moved to since
    /// stays highlighted where the new list puts it.
    pub(crate) fn files_found(&mut self, query: &str, files: Vec<String>) -> Option<KeyEvent> {
        let held = self.file_search.held.take();
        if self.shell_mode || self.composer.text().starts_with('/') {
            return None;
        }
        if self
            .composer
            .mention_query()
            .is_none_or(|(_, current)| current != query)
        {
            return None;
        }
        let moved_to = (self.selection != 0)
            .then(|| self.mention_suggestions.get(self.selection).cloned())
            .flatten();
        self.show_files(query, files);
        if let Some(at) = moved_to.and_then(|row| {
            self.mention_suggestions
                .iter()
                .position(|suggestion| *suggestion == row)
        }) {
            self.selection = at;
        }
        // Only to the prompt it was pressed in: never to a dialog or an
        // approval card that took the keyboard since.
        held.filter(|_| {
            matches!(self.overlay, crate::state::Overlay::None) && !self.permission_focus
        })
    }
}

/// What the event loop does when the search on its way answers: shows it,
/// and handles the Tab or Enter held for it.
#[cfg(test)]
pub(crate) async fn answered(app: &mut App, client: &ApiClient) {
    let (query, files) =
        tokio::time::timeout(std::time::Duration::from_secs(5), app.file_search.answer())
            .await
            .expect("the file search did not answer");
    if let Some(key) = app.files_found(&query, files) {
        crate::input::handle_key(app, key, client).await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::answered;
    use crate::{api::ApiClient, state::App};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// Slower than anything a person waits for before Tab or Enter.
    const SLOW: Duration = Duration::from_millis(800);

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
        let agent = |id: &str| haarness_protocol::AgentProfile {
            id: id.into(),
            name: id.into(),
            description: format!("{id} agent"),
            hidden: false,
            max_subagents: 0,
            mode: Some("primary".into()),
            model: None,
        };
        App::new(
            thread.clone(),
            vec![thread],
            vec![],
            vec![],
            vec![agent("build"), agent("plan")],
        )
    }

    /// A server whose file search answers a query `./x…` with the folders
    /// `./x…/` and `./x-other/` only after [`SLOW`], never a query holding
    /// `slow`, and any other at once with `<query>.md`.
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
                        Some(query) if query.starts_with("./x") => {
                            tokio::time::sleep(SLOW).await;
                            format!(r#"["{query}/", "./x-other/"]"#)
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

    async fn typed(app: &mut App, client: &ApiClient, text: &str) {
        for character in text.chars() {
            press(app, client, KeyCode::Char(character)).await;
        }
    }

    #[tokio::test]
    async fn typing_goes_on_while_a_file_search_does_not_answer() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "@slow").await;
        assert_eq!(app.composer.text(), "@slow");
        assert!(app.file_search.is_pending());
        // Typing on abandons the search that hangs; the next one answers.
        for _ in 0..3 {
            press(&mut app, &client, KeyCode::Backspace).await;
        }
        assert_eq!(app.composer.text(), "@s");
        answered(&mut app, &client).await;
        assert_eq!(app.mention_suggestions, ["s.md"]);
        assert!(!app.file_search.is_pending());
    }

    /// Tab pressed before a slow answer to the query typed picks from that
    /// answer once it comes, and never switches the agent meanwhile.
    #[tokio::test]
    async fn tab_before_a_slow_answer_picks_from_it() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "@./xa").await;
        press(&mut app, &client, KeyCode::Tab).await;
        assert_eq!(app.composer.text(), "@./xa");
        assert_eq!(app.agent(), "build");
        answered(&mut app, &client).await;
        assert_eq!(app.composer.text(), "@./xa/");
        assert_eq!(app.agent(), "build");
    }

    /// Enter pressed before a slow answer picks from it too: the half-typed
    /// message is never sent.
    #[tokio::test]
    async fn enter_before_a_slow_answer_picks_and_sends_nothing() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "hi @./xa").await;
        press(&mut app, &client, KeyCode::Enter).await;
        assert_eq!(app.composer.text(), "hi @./xa");
        answered(&mut app, &client).await;
        assert_eq!(app.composer.text(), "hi @./xa/");
    }

    /// With the list of `@./x1` showing, `7` and Enter pick from the answer
    /// for `@./x17`, never the row the list showed for `@./x1`.
    #[tokio::test]
    async fn a_pick_never_takes_a_row_that_answers_another_query() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "@./x1").await;
        answered(&mut app, &client).await;
        assert_eq!(app.mention_suggestions, ["./x1/", "./x-other/"]);
        typed(&mut app, &client, "7").await;
        press(&mut app, &client, KeyCode::Enter).await;
        assert_eq!(app.composer.text(), "@./x17");
        answered(&mut app, &client).await;
        assert_eq!(app.composer.text(), "@./x17/");

        // A query changed without a search (as Delete does) is asked about
        // before Tab picks.
        app.composer.set_text("@./x2".into());
        app.mention_suggestions = vec!["./x17/".into()];
        press(&mut app, &client, KeyCode::Tab).await;
        assert_eq!(app.composer.text(), "@./x2");
        answered(&mut app, &client).await;
        assert_eq!(app.composer.text(), "@./x2/");
    }

    /// Typing on lets a held Tab go: the answer that comes then only shows.
    #[tokio::test]
    async fn typing_on_lets_a_held_tab_go() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "@./xa").await;
        press(&mut app, &client, KeyCode::Tab).await;
        typed(&mut app, &client, "b").await;
        answered(&mut app, &client).await;
        assert_eq!(app.composer.text(), "@./xab");
        assert_eq!(app.mention_suggestions, ["./xab/", "./x-other/"]);
        assert_eq!(app.agent(), "build");
    }

    /// While the files are on their way the list counts as open: Up stays
    /// in it rather than bringing back a sent prompt, and Esc closes it,
    /// for good, rather than reaching a running turn.
    #[tokio::test]
    async fn up_and_esc_act_on_a_list_still_waiting_for_its_files() {
        let mut app = app();
        app.composer =
            crate::input::composer::Composer::with_history(vec!["an earlier prompt".into()]);
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "@slow").await;
        press(&mut app, &client, KeyCode::Up).await;
        assert_eq!(app.composer.text(), "@slow");
        app.active_turn = Some(haarness_protocol::TurnId::new());
        press(&mut app, &client, KeyCode::Esc).await;
        assert!(app.autocomplete_hidden);
        assert!(!app.file_search.is_pending());
        assert!(app.escape_interrupt.is_none());
        assert_ne!(app.notice, "Press Esc again to interrupt");
    }

    #[test]
    fn an_answer_shows_only_while_the_prompt_holds_its_query() {
        let mut app = app();
        app.composer.set_text("@new".into());
        assert_eq!(app.files_found("old", vec!["old.md".into()]), None);
        assert!(app.mention_suggestions.is_empty());
        assert_eq!(
            app.files_found("new", vec!["new.md".into(), "news.md".into()]),
            None
        );
        assert_eq!(app.mention_suggestions, ["new.md", "news.md"]);
        assert_eq!(app.selection, 0);

        // A row the person moved to stays highlighted where the new list
        // puts it.
        app.move_selection(1);
        let _ = app.files_found("new", vec!["renewed.md".into(), "news.md".into()]);
        assert_eq!(app.mention_suggestions[app.selection], "news.md");

        // Nothing shows once the query has gone.
        app.composer.set_text("plain".into());
        let _ = app.files_found("new", vec!["later.md".into()]);
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
