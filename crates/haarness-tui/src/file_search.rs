//! The `@` list's file search, off the key path. Each edit of an `@` query
//! asks the server again and abandons the search before it; an answer is
//! shown when it comes, and only while the prompt still holds the query it
//! answers. A search that is slow or never answers (a folder on an offline
//! network volume, a server far off or busy) never holds up typing: the list
//! shows the last answer until a newer one comes. Before the first, it
//! shows the agents that match the query typed, or nothing (rather than
//! "No matches").
//!
//! Tab and Enter pick only from rows that answer the query typed. Pressed
//! while that answer is on its way, the key is held and handled as soon as
//! the answer is shown, as if it had come at once; what is typed meanwhile
//! waits behind it and follows in order. Esc or a click lets the held key
//! go, as does a wait past [`QUEUED_AT_MOST`] once something waits behind
//! it. Up, Down and Esc treat the list as open while its files are on their
//! way.
use crate::{api::ApiClient, state::App};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// How long what is typed behind a held Tab or Enter waits for it; then the
/// held key is let go and what was typed is handled.
const QUEUED_AT_MOST: Duration = Duration::from_secs(2);

/// The search on its way, if any, what the list shows, and a held key.
#[derive(Default)]
pub(crate) struct FileSearch {
    /// The `@` query asked about, and the task asking.
    pending: Option<(String, JoinHandle<Vec<String>>)>,
    /// The `@` query the rows of the list answer.
    shown: Option<String>,
    /// The list shows only the agents that match while the first answer is
    /// on its way: each edit shows those of the query typed.
    agents_only: bool,
    /// A Tab or Enter pressed while the answer to the query typed was on its
    /// way, handled once it is shown.
    held: Option<KeyEvent>,
    /// The keys and pastes that came after the held key, in order, and when
    /// the first of them came.
    queued: Vec<Event>,
    queued_since: Option<Instant>,
}

/// What the event loop does with a terminal event (see [`FileSearch::pass`]).
pub(crate) enum Pass {
    /// Handle it now.
    Now(Event),
    /// Handle these first, in order, each passed again; this event is
    /// among them or waits behind a held key.
    Later(Vec<Event>),
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

    /// Every terminal event passes here first. Without a held key it is
    /// handled now. Behind one, a key or a paste waits; Esc or a click lets
    /// the held key go and comes after what waited; anything else (a
    /// resize, the pointer moving) is handled now.
    pub(crate) fn pass(&mut self, event: Event) -> Pass {
        if self.held.is_none() {
            return Pass::Now(event);
        }
        if self.pending.is_none() {
            return Pass::Later(self.let_go(Some(event)));
        }
        match &event {
            Event::Key(key) if key.kind == KeyEventKind::Release => Pass::Now(event),
            Event::Key(key) if key.code == KeyCode::Esc => Pass::Later(self.let_go(Some(event))),
            Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Down(_)) => {
                Pass::Later(self.let_go(Some(event)))
            }
            Event::Key(_) | Event::Paste(_) => {
                self.queued_since.get_or_insert_with(Instant::now);
                self.queued.push(event);
                Pass::Later(Vec::new())
            }
            _ => Pass::Now(event),
        }
    }

    /// What waited behind a held key that waited too long, the held key let
    /// go: see [`QUEUED_AT_MOST`]. Nothing while the wait goes on.
    pub(crate) fn overdue(&mut self) -> Vec<Event> {
        let late = self
            .queued_since
            .is_some_and(|since| since.elapsed() >= QUEUED_AT_MOST);
        if self.held.is_some() && (late || self.pending.is_none()) {
            self.let_go(None)
        } else {
            Vec::new()
        }
    }

    /// Lets go of the held key: what waited behind it, then `event`.
    fn let_go(&mut self, event: Option<Event>) -> Vec<Event> {
        self.held = None;
        self.queued_since = None;
        let mut events = std::mem::take(&mut self.queued);
        events.extend(event);
        events
    }
}

/// Asks the server, off the key path, for the files that complete the `@`
/// query `query`; the event loop shows the answer (see [`App::files_found`]).
/// A list that showed nothing shows the agents that match meanwhile.
pub(crate) fn search(app: &mut App, client: &ApiClient, query: String) {
    let (path, _) = crate::prompt_context::range(&query);
    let asking = {
        let client = client.clone();
        let path = path.to_owned();
        async move { client.search_files(&path, 8).await.unwrap_or_default() }
    };
    app.file_search.stop();
    app.selection = 0;
    if app.mention_suggestions.is_empty() || app.file_search.agents_only {
        app.mention_suggestions = app.agent_rows(&query);
        app.file_search.agents_only = true;
        app.list_generation += 1;
    }
    app.file_search.pending = Some((query, tokio::spawn(asking)));
}

/// Whether Tab or Enter (`key`) waits to pick from the `@` list: while the
/// answer to the query typed is on its way, or when the list shows rows that
/// answer another query (the query typed is then asked about). The key is
/// then held and handled once that answer is shown, instead of switching
/// the agent or sending the message meanwhile. With no list open and
/// nothing on its way (the text came back from history, was pasted, or the
/// cursor moved into it), the key does what it does without a list.
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
        if app.mention_suggestions.is_empty()
            || app.file_search.shown.as_deref() == Some(query.as_str())
        {
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

    /// The `@` rows of the agents that `query` names.
    fn agent_rows(&self, query: &str) -> Vec<String> {
        self.agents
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
            .collect()
    }

    /// The `@` list for `query` with `files`, the server's answer: the
    /// agents that match, then the files, then the MCP resources.
    pub(crate) fn show_files(&mut self, query: &str, files: Vec<String>) {
        let (_, range) = crate::prompt_context::range(query);
        let mut suggestions = self.agent_rows(query);
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
        self.file_search.agents_only = false;
        self.list_generation += 1;
        self.selection = 0;
        self.select_wanted();
    }

    /// A search off the key path answered `query` with `files`: shown while
    /// the prompt still holds that query. Returns what the event loop
    /// handles next, in order: the Tab or Enter held for this answer, when it
    /// was shown, then what was typed behind it. A row the person moved to
    /// since stays highlighted where the new list puts it.
    pub(crate) fn files_found(&mut self, query: &str, files: Vec<String>) -> Vec<Event> {
        let shown = !self.shell_mode
            && !self.composer.text().starts_with('/')
            && self
                .composer
                .mention_query()
                .is_some_and(|(_, current)| current == query);
        if shown {
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
        }
        // Only to the prompt it was pressed in: never to a dialog or an
        // approval card that took the keyboard since.
        let held = self.file_search.held.take().filter(|_| {
            shown && matches!(self.overlay, crate::state::Overlay::None) && !self.permission_focus
        });
        let mut next: Vec<Event> = held.map(Event::Key).into_iter().collect();
        next.extend(self.file_search.let_go(None));
        next
    }
}

/// The event loop's path for `events`, as in `crate::run_with_client`: each
/// passes [`FileSearch::pass`], and keys are handled.
#[cfg(test)]
pub(crate) async fn handle(app: &mut App, client: &ApiClient, events: Vec<Event>) {
    let mut next: std::collections::VecDeque<Event> = events.into();
    while let Some(event) = next.pop_front() {
        match app.file_search.pass(event) {
            Pass::Now(Event::Key(key)) => {
                crate::input::handle_key(app, key, client).await.unwrap();
            }
            Pass::Now(Event::Paste(text)) => app.composer.paste(&text),
            Pass::Now(_) => {}
            Pass::Later(first) => {
                for event in first.into_iter().rev() {
                    next.push_front(event);
                }
            }
        }
    }
}

/// What the event loop does when the search on its way answers: shows it,
/// then handles the Tab or Enter held for it and what waited behind.
#[cfg(test)]
pub(crate) async fn answered(app: &mut App, client: &ApiClient) {
    let (query, files) =
        tokio::time::timeout(std::time::Duration::from_secs(5), app.file_search.answer())
            .await
            .expect("the file search did not answer");
    let next = app.files_found(&query, files);
    handle(app, client, next).await;
}

#[cfg(test)]
mod tests {
    use super::{answered, handle};
    use crate::{api::ApiClient, state::App};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
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
        let agent = |id: &str, mode: &str| haarness_protocol::AgentProfile {
            id: id.into(),
            name: id.into(),
            description: format!("{id} agent"),
            hidden: false,
            max_subagents: 0,
            mode: Some(mode.into()),
            model: None,
        };
        let mut app = App::new(
            thread.clone(),
            vec![thread],
            vec![],
            vec![],
            vec![
                agent("build", "primary"),
                agent("plan", "primary"),
                agent("explore", "subagent"),
            ],
        );
        app.providers = vec![haarness_protocol::ProviderInfo {
            id: "echo".into(),
            name: "Echo".into(),
            authenticated: true,
            models: Vec::new(),
        }];
        app
    }

    /// A server whose file search answers a query `./x…` with the folders
    /// `./x…/` and `./x-other/` only after [`SLOW`], `none` with no files,
    /// never a query holding `slow`, and any other at once with
    /// `<query>.md`; that starts a turn when asked to; and that answers
    /// anything else with `[]`.
    async fn searching() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
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
                        Some("none") => "[]".to_owned(),
                        Some(query) => format!(r#"["{query}.md"]"#),
                        None if text.starts_with("POST /v1/threads/")
                            && text.contains("/turns ") =>
                        {
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
                        }
                        None if text.starts_with("GET /v1/info") => {
                            serde_json::to_string(&haarness_protocol::ServerInfo {
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
                            })
                            .unwrap()
                        }
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

    /// Presses `code` as the event loop passes it on, which must take less
    /// than a second.
    async fn press(app: &mut App, client: &ApiClient, code: KeyCode) {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        tokio::time::timeout(
            Duration::from_secs(1),
            handle(app, client, vec![Event::Key(key)]),
        )
        .await
        .expect("a key waited on the file search");
    }

    async fn typed(app: &mut App, client: &ApiClient, text: &str) {
        for character in text.chars() {
            press(app, client, KeyCode::Char(character)).await;
        }
    }

    fn screen(app: &App) -> String {
        app.lines((100, 30))
            .iter()
            .map(haarness_opentui::Line::text)
            .collect::<Vec<_>>()
            .join("\n")
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

        // A query changed under rows that answer another (as Delete does)
        // is asked about before Tab picks.
        app.composer.set_text("@./x2".into());
        app.mention_suggestions = vec!["./x17/".into()];
        press(&mut app, &client, KeyCode::Tab).await;
        assert_eq!(app.composer.text(), "@./x2");
        answered(&mut app, &client).await;
        assert_eq!(app.composer.text(), "@./x2/");
    }

    /// What is typed behind a held Tab waits for it and follows in order,
    /// as if the answer had come at once; Esc lets the Tab go and what was
    /// typed before it still comes.
    #[tokio::test]
    async fn keys_typed_behind_a_held_tab_follow_it() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "@./xa").await;
        press(&mut app, &client, KeyCode::Tab).await;
        typed(&mut app, &client, " thanks").await;
        assert_eq!(app.composer.text(), "@./xa");
        answered(&mut app, &client).await;
        assert_eq!(app.composer.text(), "@./xa/ thanks");
        assert_eq!(app.agent(), "build");

        app.composer.set_text(String::new());
        app.mention_suggestions.clear();
        typed(&mut app, &client, "@./xb").await;
        press(&mut app, &client, KeyCode::Tab).await;
        typed(&mut app, &client, "c").await;
        press(&mut app, &client, KeyCode::Esc).await;
        assert_eq!(app.composer.text(), "@./xbc");
        assert!(app.autocomplete_hidden);
        assert!(!app.file_search.is_pending());
        assert_eq!(app.agent(), "build");
    }

    /// What waits behind a held Tab no longer waits once the answer is
    /// [`super::QUEUED_AT_MOST`] late: the Tab is let go, without switching
    /// the agent, and what was typed is handled.
    #[tokio::test]
    async fn keys_behind_a_held_tab_wait_only_so_long() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        typed(&mut app, &client, "@slow").await;
        press(&mut app, &client, KeyCode::Tab).await;
        typed(&mut app, &client, "!").await;
        assert_eq!(app.composer.text(), "@slow");
        assert!(app.file_search.overdue().is_empty());
        app.file_search.queued_since =
            std::time::Instant::now().checked_sub(super::QUEUED_AT_MOST + Duration::from_millis(1));
        let next = app.file_search.overdue();
        handle(&mut app, &client, next).await;
        assert_eq!(app.composer.text(), "@slow!");
        assert_eq!(app.agent(), "build");
        assert!(app.file_search.held.is_none());
    }

    /// `@` text no list was opened for (it came back from history, was
    /// pasted, or the cursor moved into it) leaves Enter and Tab as they
    /// are without a list: Enter sends, Tab switches the agent.
    #[tokio::test]
    async fn enter_and_tab_on_text_no_list_opened_for_act_as_without_one() {
        let client = ApiClient::new(&searching().await, "token").unwrap();

        let mut recalled = app();
        let thread = recalled.thread.id.to_string();
        recalled.composer.set_history_owner(
            format!("{}:", client.server_url()),
            thread,
            vec!["hi @./xa".into()],
        );
        press(&mut recalled, &client, KeyCode::Up).await;
        assert_eq!(recalled.composer.text(), "hi @./xa");
        press(&mut recalled, &client, KeyCode::Enter).await;
        assert_eq!(
            recalled.composer.text(),
            "",
            "the recalled prompt was not sent"
        );
        assert!(!recalled.file_search.is_pending());

        let mut pasted = app();
        handle(&mut pasted, &client, vec![Event::Paste("cc @./xa".into())]).await;
        press(&mut pasted, &client, KeyCode::Enter).await;
        assert_eq!(pasted.composer.text(), "", "the pasted prompt was not sent");

        let mut moved = app();
        typed(&mut moved, &client, "see @./xa now").await;
        for _ in 0..4 {
            press(&mut moved, &client, KeyCode::Left).await;
        }
        assert!(moved.composer.mention_query().is_some());
        press(&mut moved, &client, KeyCode::Enter).await;
        assert_eq!(moved.composer.text(), "", "the prompt was not sent");

        let mut tabbed = app();
        handle(&mut tabbed, &client, vec![Event::Paste("@./xa".into())]).await;
        press(&mut tabbed, &client, KeyCode::Tab).await;
        assert_eq!(tabbed.agent(), "plan");
        assert_eq!(tabbed.composer.text(), "@./xa");
    }

    /// While the files are on their way the list shows the agents that
    /// match at once, and nothing at all when none do: "No matches" only
    /// once the answer says so.
    #[tokio::test]
    async fn a_list_waiting_for_its_files_never_says_no_matches() {
        let client = ApiClient::new(&searching().await, "token").unwrap();
        let mut app = app();
        app.agents.retain(|agent| agent.id != "explore");
        typed(&mut app, &client, "@slow").await;
        let waiting = screen(&app);
        assert!(!waiting.contains("No matches"), "{waiting}");
        assert!(
            !waiting.lines().any(|line| line.trim() == "files · agents"),
            "{waiting}"
        );

        let mut app = self::app();
        typed(&mut app, &client, "@").await;
        assert!(app.file_search.is_pending());
        assert_eq!(app.mention_suggestions, ["agent://explore"]);
        assert!(screen(&app).contains("@explore"));
        // Before the first answer, the agents follow the query typed.
        typed(&mut app, &client, "slow").await;
        assert!(app.mention_suggestions.is_empty());
        let waiting = screen(&app);
        assert!(!waiting.contains("@explore"), "{waiting}");
        assert!(!waiting.contains("No matches"), "{waiting}");

        let mut app = self::app();
        typed(&mut app, &client, "@none").await;
        answered(&mut app, &client).await;
        assert!(screen(&app).contains("No matches"));
    }

    /// While the files are on their way the list counts as open: Up stays
    /// in it rather than bringing back a sent prompt, and Esc closes it,
    /// for good, rather than reaching a running turn.
    #[tokio::test]
    async fn up_and_esc_act_on_a_list_still_waiting_for_its_files() {
        let mut app = app();
        let client = ApiClient::new(&searching().await, "token").unwrap();
        let thread = app.thread.id.to_string();
        app.composer.set_history_owner(
            format!("{}:", client.server_url()),
            thread,
            vec!["@slow and an earlier prompt".into()],
        );
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
        assert!(app.files_found("old", vec!["old.md".into()]).is_empty());
        assert!(app.mention_suggestions.is_empty());
        assert!(
            app.files_found("new", vec!["new.md".into(), "news.md".into()])
                .is_empty()
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
