//! Elegant server-driven terminal client rendered through OpenTUI.

pub mod api;
mod chrome;
mod commands;
mod config;
mod connect;
mod diff;
mod diffview;
mod highlight;
mod input;
mod input_queue;
mod keybindings;
mod locale;
mod logs;
mod management;
mod markdown;
mod media;
mod messages;
mod preferences;
mod prompt_context;
mod pty_view;
mod remote_control;
mod resource_mentions;
mod servers;
mod side_chat;
mod state;
mod statusline;
mod terminal;
mod theme;
mod theme_embedded;
mod view;
mod width;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use crossterm::event::Event as TerminalEvent;
use haarness_opentui::{Color, ColorSupport, Renderer};
use haarness_protocol::CreateThreadRequest;
use std::{path::PathBuf, time::Duration};

/// How long the first frame waits for the provider catalog.
const PROVIDERS_FIRST_FRAME: Duration = Duration::from_secs(1);

use crate::api::{ApiClient, EndpointResolver, ServerRestarter};
use crate::input::{AppAction, handle_key};
/// The model a new chat starts on when none is configured; `haarness run`
/// picks the same way.
pub use crate::state::default_connected_model;
use crate::state::{App, ToastVariant};
use crate::terminal::TerminalGuard;

/// Pick the page-clear color independently from component surfaces. In auto
/// mode, limited-color terminals retain their configured background instead
/// of having an approximate ANSI palette entry painted over the whole screen.
pub(crate) const fn page_background(
    theme_background: Color,
    mode: config::BackgroundMode,
    support: ColorSupport,
) -> Color {
    if uses_terminal_background(mode, support) {
        Color::TRANSPARENT
    } else {
        theme_background
    }
}

pub(crate) const fn uses_terminal_background(
    mode: config::BackgroundMode,
    support: ColorSupport,
) -> bool {
    matches!(mode, config::BackgroundMode::Terminal)
        || matches!(mode, config::BackgroundMode::Auto) && !support.truecolor
}

pub(crate) fn initial_theme_variant(
    terminal_background: bool,
    detected: Option<&str>,
    saved: Option<&str>,
) -> String {
    if terminal_background {
        detected.or(saved)
    } else {
        saved.or(detected)
    }
    .unwrap_or("dark")
    .to_owned()
}

/// Restore durable goal state before the first frame is rendered. Goal SSE
/// updates only cover changes after the captured event cursor, so relying on
/// the live stream alone leaves an existing pursuit invisible on launch.
pub(crate) async fn hydrate_goal_snapshot(app: &mut App, client: &ApiClient) -> bool {
    match client.goal(app.thread.id).await {
        Ok(goal) => {
            app.install_goal_snapshot(goal);
            true
        }
        Err(_) => {
            app.goal_refresh_needed = true;
            false
        }
    }
}

/// The event task's restarts after it ended by itself: three within a minute.
#[derive(Default)]
struct Restarts(Vec<std::time::Instant>);

impl Restarts {
    /// Whether it may start again now.
    fn again(&mut self) -> Option<()> {
        let now = std::time::Instant::now();
        self.0
            .retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        (self.0.len() < 3).then(|| self.0.push(now))
    }
}

async fn hydrate_initial_goal(app: &mut App, client: &ApiClient) {
    hydrate_goal_snapshot(app, client).await;
}

/// The user's editor on `file`. `$EDITOR` is shell text (`code --wait`); the
/// file goes to the shell as `$1`, so a temporary folder holding a space or
/// `$(…)` is never read as shell syntax.
fn editor_command(editor: &str, file: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command
        .arg("-lc")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(file);
    command
}

/// $EDITOR round-trip: leave the alternate screen, run the user's editor on
/// the composer text, and rebuild the renderer when they return.
fn open_external_editor(app: &mut App, renderer: &mut Renderer) {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let file = std::env::temp_dir().join(format!("haarness-compose-{}.md", std::process::id()));
    let _ = std::fs::write(&file, app.composer.expanded_text());
    crate::terminal::restore_terminal();
    let status = editor_command(&editor, &file).status();
    // Re-enter raw mode + alternate screen; the original guard still owns
    // final cleanup, so resuming must not create and leak a second guard.
    let _ = crate::terminal::resume_terminal();
    if let Ok((width, height)) = crossterm::terminal::size() {
        renderer.resize(u32::from(width), u32::from(height));
    }
    match status {
        Ok(status) if status.success() => {
            if let Ok(content) = std::fs::read_to_string(&file) {
                app.composer.set_text(content.trim_end().to_owned());
                app.notice = "Editor content loaded — Enter sends".into();
            }
        }
        _ => app.notice = "Editor cancelled".into(),
    }
    let _ = std::fs::remove_file(&file);
}

fn suspend_terminal(renderer: &mut Renderer, last_title: &mut String) -> Result<()> {
    crate::terminal::suspend_and_resume()?;
    let (width, height) =
        crossterm::terminal::size().context("read terminal dimensions after resuming Haarness")?;
    renderer.resize(u32::from(width), u32::from(height));
    // restore_terminal clears OSC 0; invalidate the cache so the next frame
    // restores the thread title too.
    last_title.clear();
    Ok(())
}

/// Attach to an explicitly selected endpoint. Local media is uploaded into
/// requests, and `fork` clones the selected or continued session before the
/// first frame so the original remains untouched.
pub async fn run_attached_with_auth(
    server: &str,
    auth: api::ServerAuth,
    initial_thread: Option<uuid::Uuid>,
    continue_last: bool,
    fork: bool,
    open_in_auto: bool,
) -> Result<()> {
    Box::pin(run_with_client(
        ApiClient::with_auth(server, auth)?.with_attachment_uploads(),
        initial_thread,
        continue_last,
        fork,
        open_in_auto,
    ))
    .await
    .map(|_| ())
}

/// Cloud attachment tickets renew independently of the remote runtime.
pub async fn run_attached_with_resolver(
    server: &str,
    token: &str,
    resolver: EndpointResolver,
) -> Result<()> {
    Box::pin(run_with_client(
        ApiClient::new(server, token)?
            .with_remote_endpoint_resolver(resolver)
            .with_attachment_uploads(),
        None,
        false,
        false,
        false,
    ))
    .await
    .map(|_| ())
}

/// Resume a Cloud session's most recent conversation, creating one only for a new session.
pub async fn resume_attached_with_resolver(
    server: &str,
    token: &str,
    resolver: EndpointResolver,
) -> Result<()> {
    Box::pin(run_with_client(
        ApiClient::new(server, token)?
            .with_remote_endpoint_resolver(resolver)
            .with_attachment_uploads(),
        None,
        true,
        false,
        false,
    ))
    .await
    .map(|_| ())
}

/// Start a local, auto-discovered TUI that can follow a replacement daemon's
/// new loopback port and bearer token (explicit remote `--connect` clients use
/// `run_attached_with_auth` and stay pinned to the endpoint the user chose).
/// A plain launch creates a fresh root thread and home screen;
/// `continue_last` restores the saved/most-recent root, and an explicit
/// `--thread`/`--session` id wins over both. Returns a workspace path the user
/// switched to, once the terminal has been restored, so the CLI can start
/// that workspace's own server without nesting TUIs.
/// `restarter` brings the workspace server back if it stops or crashes while
/// the TUI is open. `approvals` says how the launch answers approvals.
pub async fn run_with_thread_and_resolver_switching(
    server: &str,
    token: &str,
    initial_thread: Option<uuid::Uuid>,
    continue_last: bool,
    resolver: EndpointResolver,
    restarter: ServerRestarter,
    approvals: ApprovalLaunch,
) -> Result<Option<PathBuf>> {
    run_with_thread_and_resolver_fork_switching(
        server,
        token,
        initial_thread,
        continue_last,
        false,
        resolver,
        restarter,
        approvals,
    )
    .await
}

/// How a launch answers approvals, from its command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApprovalLaunch {
    /// `--yolo`: this TUI approves what its own conversations ask, for as
    /// long as it runs (see [`ApiClient::with_auto_approve`]).
    pub yolo: bool,
    /// `--auto`: the chat the launch opens goes to Auto on its server, so
    /// every app shows it and the server answers its eligible requests.
    pub auto: bool,
}

/// As [`run_with_thread_and_resolver_switching`], forking the selected or
/// continued session first so the original remains untouched.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_thread_and_resolver_fork_switching(
    server: &str,
    token: &str,
    initial_thread: Option<uuid::Uuid>,
    continue_last: bool,
    fork: bool,
    resolver: EndpointResolver,
    restarter: ServerRestarter,
    approvals: ApprovalLaunch,
) -> Result<Option<PathBuf>> {
    Box::pin(run_with_client(
        ApiClient::new(server, token)?
            .with_endpoint_resolver(resolver)
            .with_server_restarter(restarter)
            .with_auto_approve(approvals.yolo),
        initial_thread,
        continue_last,
        fork,
        approvals.auto,
    ))
    .await
}

async fn run_with_client(
    mut client: ApiClient,
    initial_thread: Option<uuid::Uuid>,
    continue_last: bool,
    fork: bool,
    auto: bool,
) -> Result<Option<PathBuf>> {
    let initial_info = client.info().await.ok();
    // The server waits for a signed-in provider's list while it has no
    // model to show (up to 10 s on a stalled network): the first frame does
    // not, and the list comes in after it (see `App::catalog_arrived`).
    let (mut providers, models_loading) =
        match tokio::time::timeout(PROVIDERS_FIRST_FRAME, client.providers()).await {
            Ok(listed) => (listed.unwrap_or_default(), false),
            Err(_) => (Vec::new(), true),
        };
    let mut chose_model = false;
    let mut threads = client.threads().await?;
    let saved = config::load_state();
    let saved_thread = saved
        .last_thread
        .as_deref()
        .and_then(|value| value.parse().ok());
    let restored = choose_initial_thread(&threads, initial_thread, saved_thread, continue_last);
    let thread = if let Some(thread) = restored {
        if fork {
            client.fork_thread(thread.id).await?
        } else {
            thread
        }
    } else {
        if fork {
            anyhow::bail!("--fork requires an existing --session/--thread or --continue target");
        }
        let mut request = CreateThreadRequest {
            agent: initial_info
                .as_ref()
                .and_then(|info| info.default_agent.clone()),
            ..Default::default()
        };
        let mut provider_overridden = false;
        if let Ok(provider) = std::env::var("HAARNESS_DEFAULT_PROVIDER") {
            request.provider = Some(provider);
            provider_overridden = true;
        }
        if let Ok(model) = std::env::var("HAARNESS_DEFAULT_MODEL") {
            request.model = Some(model);
        } else if provider_overridden {
            if let Some(model) = providers
                .iter()
                .find(|provider| Some(&provider.id) == request.provider.as_ref())
                .and_then(|provider| provider.models.first())
            {
                request.model = Some(model.id.clone());
            }
        } else if let Some((provider, model)) = initial_info
            .as_ref()
            .and_then(|info| Some((info.default_provider.clone()?, info.default_model.clone()?)))
        {
            request.provider = Some(provider);
            request.model = Some(model);
        } else if let Some((provider, model)) = default_connected_model(&providers) {
            request.provider = Some(provider);
            request.model = Some(model);
            chose_model = true;
        } else {
            chose_model = true;
        }
        let thread = client.create_thread(request).await?;
        threads.push(thread.clone());
        thread
    };
    // Capture the event boundary before snapshot hydration. SSE then replays
    // exactly the deltas that raced these REST reads.
    let server_info = client.info().await.ok();
    // The server answers with the model lists it has and announces the ones
    // still loading when they come (`assets.updated`). One that came before
    // this point did so before the events this client follows: ask again,
    // without waiting for the rest.
    if !models_loading
        && let Ok(Ok(current)) =
            tokio::time::timeout(Duration::from_millis(300), client.providers()).await
    {
        providers = current;
    }
    let agents = client.agents().await.unwrap_or_default();
    let items = client.items(thread.id).await.unwrap_or_default();
    let restored_view = saved.thread_views.get(&thread.id.to_string());
    let restored_scroll = restored_view.map_or_else(
        || {
            (Some(thread.id.to_string()) == saved.last_thread)
                .then_some(saved.viewport_offset)
                .flatten()
                .unwrap_or(0)
        },
        |view| view.viewport_offset,
    );
    // The chat this TUI opened on the model it chose takes the one it would
    // have chosen with the lists still loading, once they come.
    let auto_model = (models_loading && chose_model)
        .then(|| (thread.id, thread.provider.clone(), thread.model.clone()));
    let mut app = App::new(thread, threads, items, providers, agents);
    app.models_loading = models_loading;
    app.auto_model = auto_model;
    if models_loading {
        app.notice = "Loading models…".into();
    }
    app.yolo = client.auto_approves();
    app.input_queue = client.input_queue(app.thread.id).await.unwrap_or_default();
    app.original_client = Some(client.clone());
    app.workspace_local_switching = client.supports_local_workspace_switch();
    if let Ok(todos) = client.todos(app.thread.id).await {
        app.todos = todos;
    }
    hydrate_initial_goal(&mut app, &client).await;
    if let Ok(turns) = client.active_turns().await {
        app.hydrate_active_turns(turns);
    }
    if app.active_turn.is_some() {
        app.notice = "Running — reattached to active turn".into();
    }
    if let Ok(questions) = client.pending_questions().await {
        app.hydrate_questions(questions);
    }
    app.viewport_offset = restored_scroll;
    if let Some(view) = restored_view {
        app.expanded = view
            .expanded
            .iter()
            .filter_map(|id| id.parse().ok())
            .map(haarness_protocol::ItemId)
            .collect();
    }
    app.server_info = server_info;
    app.server_commands = client.commands().await.unwrap_or_default();
    // Workflow definitions are first-class slash commands (for example,
    // `.haarness/workflows/release.ts` becomes `/release`). Hydrate the
    // catalog before the input loop so completion and exact-command dispatch
    // work without requiring the user to open `/workflow` first.
    if let Ok(catalog) = client.workflows().await {
        app.workflow_definitions = catalog.workflows;
        app.workflow_invalid = catalog.invalid;
    }
    // Prompts raised before this client attached (e.g. by another client or a
    // still-running turn) must be answerable immediately.
    app.hydrate_permissions(
        client.pending_permissions().await.unwrap_or_default(),
        &client,
    )
    .await;
    // `--auto`: the chat opens in Auto on its server, as ctrl+y would put
    // it, so the server answers what is eligible (including what already
    // waits) and every other app shows the same mode.
    if auto {
        app.change_approval_mode(true, &client).await;
    }
    app.sync_prompt_history(&client);
    servers::restore_drafts(&mut app, &client)?;
    // First run: nothing can answer a prompt yet, so connecting is the first
    // thing to do. The picker is the welcome screen; Escape leaves it.
    if app.items.is_empty()
        && !app.any_provider_connected()
        && std::env::var_os("HAARNESS_NO_CONNECT_PROMPT").is_none()
    {
        app.connect.onboarding = true;
        if let Err(error) = app.open_connect_picker(None, &client).await {
            app.notice = error.to_string();
        }
    }
    app.themes = config::load_theme_entries();
    app.language = config::load_language();
    app.thinking = config::load_thinking();
    if !app.keybindings_skipped.is_empty() {
        app.notice = app.language.format(
            "Shortcuts not applied: {names} · /keybinds",
            &[("names", &app.keybindings_skipped.join(", "))],
        );
    }
    // A waiting prompt takes the keyboard unless a restored draft is being
    // written, as one arriving live does; ctrl+g reaches it either way.
    if !app.pending_permissions.is_empty() {
        app.permission_focus = app.composer.is_empty();
        app.update_permission_notice();
    }
    let statusline_config = config::load_statusline();
    let (saved_theme, saved_variant) = config::load_theme_selection();
    app.theme_selected = saved_theme.is_some() || saved_variant.is_some();
    let initial_theme = saved_theme.unwrap_or_else(|| theme::DEFAULT_THEME.into());
    let detected_variant = crate::terminal::detect_background_variant().map(str::to_owned);
    let terminal_guard = TerminalGuard::enter()?;
    let (width, height) = crossterm::terminal::size()?;
    let mut renderer = Renderer::new(u32::from(width), u32::from(height), false)
        .context("initialize embedded OpenTUI renderer")?;
    let color_support = renderer.color_support();
    let background_mode = config::load_background_mode();
    let initial_background_mode = background_mode.for_theme_choice(app.theme_selected);
    let terminal_background = uses_terminal_background(initial_background_mode, color_support);
    app.set_terminal_background(terminal_background);
    // A persisted picker variant is an explicit palette choice in theme mode.
    // When the terminal owns the background, its measured light/dark value
    // must win so a theme cannot strand light text on white (or vice versa).
    let initial_variant = initial_theme_variant(
        terminal_background,
        detected_variant.as_deref(),
        saved_variant.as_deref(),
    );
    if !app.apply_theme(&initial_theme, &initial_variant)
        && !app.apply_theme(theme::DEFAULT_THEME, &initial_variant)
    {
        app.apply_theme(theme::DEFAULT_THEME, "dark");
    }
    renderer.set_background(page_background(
        app.theme.bg,
        initial_background_mode,
        color_support,
    ));
    let mut input = crate::input::meta::TerminalInput::new();
    let initial_cursor = app
        .server_info
        .as_ref()
        .map(|info| (info.server_instance_id, info.event_cursor));
    let (mut event_rx, mut event_task) = client.subscribe(initial_cursor);
    // How often the event task may be started again after it ended by
    // itself; past that the indicator stays, rather than a panic looping.
    let mut following = Some(Restarts::default());
    // While it is stopped, when it is tried again.
    let mut follow_again_at: Option<std::time::Instant> = None;
    // Open completion only after SSE owns the captured cursor. If a localized
    // completion event raced snapshot hydration, its replay can now replace
    // the fallback dialog without a visible stale-copy window.
    app.reveal_goal_completion();
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    // SIGTERM/SIGHUP must unwind through TerminalGuard so the terminal is
    // restored instead of wedging in raw mode + alternate screen.
    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = signal(SignalKind::terminate()).ok();
            let mut hangup = signal(SignalKind::hangup()).ok();
            let wait_terminate = async {
                match terminate.as_mut() {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let wait_hangup = async {
                match hangup.as_mut() {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                () = wait_terminate => {},
                () = wait_hangup => {},
            }
        }
        #[cfg(not(unix))]
        std::future::pending::<()>().await
    };
    tokio::pin!(shutdown);
    // First frame paints everything; afterwards the native renderer's cell
    // diffing suppresses unchanged output. Resize forces a full repaint.
    let mut force_redraw = true;
    let mut last_title = String::new();
    let mut selected_workspace = None;
    let (workspace_mutation_tx, mut workspace_mutation_rx) = tokio::sync::mpsc::channel(1);
    let (workspace_ref_tx, mut workspace_ref_rx) = tokio::sync::mpsc::channel(1);
    let mut pty_rx = client.subscribe_ptys();
    // Provider catalogs read off the loop (the server may wait up to 10 s
    // for a list), applied when they come.
    let (catalog_tx, mut catalog_rx) = tokio::sync::mpsc::channel(4);
    app.catalog_tx = Some(catalog_tx.clone());
    // MCP resources for the `@` list, read off the loop when it first opens,
    // and messages that mention some, sent off the loop while they are read.
    let (resources_tx, mut resources_rx) = tokio::sync::mpsc::channel(16);
    app.resources_tx = Some(resources_tx);
    let (sends_tx, mut sends_rx) = tokio::sync::mpsc::channel(4);
    app.sends_tx = Some(sends_tx.clone());
    // Side chat answers being written, applied as they arrive.
    let (side_tx, mut side_rx) = tokio::sync::mpsc::unbounded_channel();
    app.side.sender = Some(side_tx.clone());
    if app.models_loading {
        let client = client.clone();
        tokio::spawn(async move {
            let _ = catalog_tx.send(client.providers().await.ok()).await;
        });
    }
    let (statusline_context, mut statusline_rx, statusline_task) =
        statusline::spawn(statusline_config);
    let _ = statusline_context.send(statusline::Context::from_app(&app));
    let mut statusline_failures = statusline::FailureLog::default();
    let mut billing = haarness_account::Config::from_env()
        .and_then(haarness_account::Client::new)
        .map(haarness_account::billing::watch)
        .ok();
    // Only leaving on purpose earns the way-back footer; a signal may mean
    // the terminal is already gone.
    let mut quit = false;
    loop {
        if let Some(connection) = app.pending_server.take() {
            match servers::save_current(&app, &client) {
                Ok(()) => {
                    event_task.abort();
                    client.disconnect_all_ptys();
                    client = connection.client;
                    app = connection.app;
                    if let Some(watch) = &billing {
                        app.billing = watch.updates.borrow().clone();
                    }
                    let initial = app
                        .server_info
                        .as_ref()
                        .map(|info| (info.server_instance_id, info.event_cursor));
                    (event_rx, event_task) = client.subscribe(initial);
                    pty_rx = client.subscribe_ptys();
                    // A catalog still on its way from the server left is not
                    // this one's.
                    let (sender, receiver) = tokio::sync::mpsc::channel(4);
                    catalog_rx = receiver;
                    app.catalog_tx = Some(sender);
                    let (sender, receiver) = tokio::sync::mpsc::channel(16);
                    resources_rx = receiver;
                    app.resources_tx = Some(sender);
                    app.sends_tx = Some(sends_tx.clone());
                    app.side.sender = Some(side_tx.clone());
                    force_redraw = true;
                }
                Err(error) => {
                    app.notice = format!("Could not preserve this server's draft: {error}");
                }
            }
        }
        let frame_background_mode = background_mode
            .for_theme_choice(app.theme_selected || app.theme_preview_restore.is_some());
        app.set_terminal_background(uses_terminal_background(
            frame_background_mode,
            color_support,
        ));
        renderer.set_background(page_background(
            app.theme.bg,
            frame_background_mode,
            color_support,
        ));
        // The review viewer's layout depends on how much room the patch pane
        // actually has, so reconcile it before the frame that reads it.
        app.sync_diff_layout(renderer.size().0 as usize);
        if let Some(terminal) = &mut app.terminal {
            let (cols, rows) = crate::pty_view::dimensions(renderer.size());
            if let Some((id, request)) = terminal.resize(cols, rows) {
                let client = client.clone();
                tokio::spawn(async move {
                    if let Err(error) = client.update_pty(id, &request).await {
                        tracing::warn!(error = %error, "PTY resize failed");
                    }
                });
            }
        }
        renderer.draw_lines(&app.lines(renderer.size()), force_redraw);
        force_redraw = false;
        // Terminal title tracks the open thread.
        let title = format!("haarness — {}", app.thread.title);
        if title != last_title {
            crate::terminal::set_title(&title);
            last_title = title;
        }
        if let Some(message) = app.pending_notification.take() {
            // A desktop banner while the terminal is in front is noise: the
            // transcript already shows it. Unfocused terminals still get it.
            if !app.terminal_focused {
                crate::terminal::notify(&message);
            }
        }
        tokio::select! {
            terminal = input.next() => if let Some(Ok(event)) = terminal {
                match event {
                    TerminalEvent::Key(key) => {
                        // A failed request behind a keystroke becomes a
                        // notice; it must never take the whole TUI down.
                        match handle_key(&mut app, key, &client).await {
                            Ok(AppAction::Quit) => {
                                quit = true;
                                break;
                            }
                            Ok(AppAction::OpenEditor) => {
                                open_external_editor(&mut app, &mut renderer);
                                force_redraw = true;
                            }
                            Ok(AppAction::Suspend) => {
                                suspend_terminal(&mut renderer, &mut last_title)?;
                                force_redraw = true;
                            }
                            Ok(AppAction::StartWorkspaceMutation(operation)) => {
                                let client = client.clone();
                                let sender = workspace_mutation_tx.clone();
                                tokio::spawn(async move {
                                    let completion =
                                        crate::commands::run_workspace_mutation(client, operation)
                                            .await;
                                    let _ = sender.send(completion).await;
                                });
                            }
                            Ok(AppAction::StartWorkspaceRefOperation(operation)) => {
                                let client = client.clone();
                                let sender = workspace_ref_tx.clone();
                                tokio::spawn(async move {
                                    let completion =
                                        crate::commands::run_workspace_ref_operation(
                                            client, operation,
                                        )
                                        .await;
                                    let _ = sender.send(completion).await;
                                });
                            }
                            Ok(AppAction::Continue) => {}
                            Err(error) => {
                                // The whole chain: a context such as "Could not
                                // start signing in" says what failed, its cause
                                // says why, and only both together say what to do.
                                tracing::warn!(
                                    error = format!("{error:#}"),
                                    status = ?crate::api::refusal_status(&error),
                                    "key handling failed"
                                );
                                app.notice = "Ready".into();
                                app.show_toast(
                                    Some("Error".into()),
                                    format!("{error:#}"),
                                    ToastVariant::Error,
                                    Duration::from_secs(7),
                                );
                            }
                        }
                        // A one-key approval answer is sent when its window
                        // ends unless another key comes first: the tick
                        // wakes the loop then rather than on its own beat.
                        if let Some(deadline) = crate::input::settle_deadline(&app) {
                            tick.reset_at(tokio::time::Instant::from_std(deadline));
                        }
                    }
                    TerminalEvent::Resize(width, height) => {
                        // Screen coordinates from an in-progress drag are no
                        // longer meaningful after a resize. Keeping them can
                        // paint or copy unrelated cells in the new geometry.
                        app.mouse_down = None;
                        app.selection_range = None;
                        app.hovered_cell = None;
                        renderer.resize(u32::from(width), u32::from(height));
                        force_redraw = true;
                    }
                    TerminalEvent::Paste(text) => {
                        if let Some(terminal) = &app.terminal {
                            if let (Some(id), Some(bytes)) =
                                (terminal.active_id(), terminal.paste_bytes(&text))
                            {
                                if let Err(error) = client.write_pty(id, bytes).await {
                                    app.notice = format!("Terminal paste failed: {error}");
                                }
                            }
                            continue;
                        }
                        if matches!(app.overlay, state::Overlay::SideChat) {
                            app.paste_side_chat(&text);
                            continue;
                        }
                        if app.overlay.is_filterable() {
                            if matches!(app.overlay, state::Overlay::Options { kind: state::OptionsKind::ManagementField, .. }) {
                                app.overlay_filter.push_str(&text.replace("\r\n", "\n"));
                            } else if matches!(app.overlay, state::Overlay::Options { kind: state::OptionsKind::ConnectApiKey, .. }) {
                                // A pasted key never wants the surrounding whitespace.
                                app.overlay_filter.push_str(text.trim());
                                app.connect.key_error = None;
                            } else {
                                app.overlay_filter.push_str(&text.replace(['\n', '\r'], " "));
                            }
                            app.selection = 0;
                            continue;
                        }
                        if app.overlay.is_selector()
                            || matches!(
                                app.overlay,
                                crate::state::Overlay::Panel { .. }
                                    | crate::state::Overlay::Diff(_)
                            )
                        {
                            continue;
                        }
                        // Bracketed paste and terminal file drops bypass the
                        // key handler entirely. A child thread is still a
                        // read-only inspector on this event path, so neither
                        // text nor attachments may create a hidden draft.
                        if app.is_child_view() {
                            continue;
                        }
                        // Pasting is writing a message: the keyboard leaves a
                        // waiting approval card, or the next Enter would
                        // answer the card instead of sending the paste.
                        if app.permission_focus {
                            app.release_permission_focus(true);
                        }
                        match crate::media::paths_from_paste(&text) {
                            Ok(Some(paths)) => {
                                let added = app.add_attachments(paths);
                                app.notice = format!(
                                    "Attached {added} file{} · backspace removes",
                                    if added == 1 { "" } else { "s" }
                                );
                            }
                            Ok(None) => app.composer.paste(&text),
                            Err(error) => app.notice = error,
                        }
                        app.selection = 0;
                    }
                    TerminalEvent::Mouse(mouse) => {
                        if let Some(terminal) = &mut app.terminal {
                            match mouse.kind {
                                crossterm::event::MouseEventKind::ScrollUp => terminal.scroll(-3),
                                crossterm::event::MouseEventKind::ScrollDown => terminal.scroll(3),
                                _ => {}
                            }
                            continue;
                        }
                        match crate::input::mouse::handle_mouse(&mut app, mouse, &client).await {
                            Ok(AppAction::Quit) => {
                                quit = true;
                                break;
                            }
                            Ok(AppAction::OpenEditor) => {
                                open_external_editor(&mut app, &mut renderer);
                                force_redraw = true;
                            }
                            Ok(AppAction::Suspend) => {
                                suspend_terminal(&mut renderer, &mut last_title)?;
                                force_redraw = true;
                            }
                            Ok(AppAction::StartWorkspaceMutation(operation)) => {
                                let client = client.clone();
                                let sender = workspace_mutation_tx.clone();
                                tokio::spawn(async move {
                                    let completion =
                                        crate::commands::run_workspace_mutation(client, operation)
                                            .await;
                                    let _ = sender.send(completion).await;
                                });
                            }
                            Ok(AppAction::StartWorkspaceRefOperation(operation)) => {
                                let client = client.clone();
                                let sender = workspace_ref_tx.clone();
                                tokio::spawn(async move {
                                    let completion =
                                        crate::commands::run_workspace_ref_operation(
                                            client, operation,
                                        )
                                        .await;
                                    let _ = sender.send(completion).await;
                                });
                            }
                            Ok(AppAction::Continue) => {}
                            Err(error) => {
                                tracing::warn!(error = %error, "mouse handling failed");
                                app.notice = format!("Error: {error}");
                            }
                        }
                    }
                    TerminalEvent::FocusGained => {
                        app.terminal_focused = true;
                        // Windows Terminal/ConPTY and some tabbed terminals can
                        // silently drop DEC mouse modes while unfocused. Re-arm
                        // them exactly when focus returns so hover never depends
                        // on whether the app was switched away from earlier.
                        if let Err(error) = crate::terminal::restore_focus_modes() {
                            tracing::warn!(error = %error, "terminal mode recovery failed");
                        }
                        app.hovered_cell = None;
                        force_redraw = true;
                    }
                    TerminalEvent::FocusLost => {
                        app.terminal_focused = false;
                        app.mouse_down = None;
                        app.selection_range = None;
                        app.hovered_cell = None;
                    }
                }
            },
            event = event_rx.recv(), if following.is_some() => {
                let Some(event) = event else {
                    // The task that follows the server's events ended by
                    // itself (it panicked): say so, and follow them again,
                    // from a fresh snapshot.
                    if following.as_mut().and_then(Restarts::again).is_some() {
                        app.connection_dropped(1);
                        (event_rx, event_task) = client.subscribe_again(Some((uuid::Uuid::nil(), 0)));
                    } else {
                        // Ending over and over: stop for a minute, and say so.
                        following = None;
                        follow_again_at = Some(std::time::Instant::now() + Duration::from_secs(60));
                        app.stopped_following();
                    }
                    continue;
                };
                match event {
                    crate::api::SubscriptionEvent::Event(event) => {
                        app.reduce(*event, &client).await?;
                    }
                    crate::api::SubscriptionEvent::ResyncRequired => {
                        // Replace REST-backed snapshots after replay loss or a
                        // server restart. Clearing deltas avoids duplicating a
                        // partial stream against the authoritative transcript.
                        app.input_queue = client.input_queue(app.thread.id).await.unwrap_or_default();
                        if let Ok(items) = client.items(app.thread.id).await {
                            app.items = items;
                            app.hydrate_child_links();
                        }
                        if let Ok(todos) = client.todos(app.thread.id).await {
                            app.todos = todos;
                        }
                        if let Ok(turns) = client.active_turns().await {
                            app.hydrate_active_turns(turns);
                        }
                        app.hydrate_questions(
                            client.pending_questions().await.unwrap_or_default(),
                        );
                        let _ = app.refresh_threads(&client).await;
                        hydrate_goal_snapshot(&mut app, &client).await;
                        app.hydrate_permissions(client.pending_permissions().await.unwrap_or_default(), &client).await;
                        // Only an empty composer gives up the keyboard to a
                        // prompt, as when it arrives live.
                        app.permission_focus = !app.pending_permissions.is_empty()
                            && (app.permission_focus || app.composer.is_empty());
                        app.reveal_goal_completion();
                        app.preview.clear();
                        app.reasoning_preview.clear();
                        app.live_threads.remove(&app.thread.id);
                        app.notice = "Reconnected — state refreshed".into();
                    }
                    crate::api::SubscriptionEvent::ConnectionLost { attempt } => {
                        app.connection_dropped(attempt);
                    }
                    crate::api::SubscriptionEvent::ServerRestarting => {
                        app.server_restart = Some(state::ServerRestart::Starting);
                    }
                    crate::api::SubscriptionEvent::ServerRestartFailed(error) => {
                        if app.server_restart != Some(state::ServerRestart::Failed) {
                            app.show_toast(
                                Some(app.language.text("Could not restart the server").into()),
                                error,
                                ToastVariant::Error,
                                Duration::from_secs(8),
                            );
                        }
                        app.server_restart = Some(state::ServerRestart::Failed);
                    }
                    crate::api::SubscriptionEvent::Reconnected => app.reconnected(),
                }
            },
            Ok(event) = pty_rx.recv() => {
                if let Some(terminal) = &mut app.terminal {
                    let exited = match event {
                        crate::api::PtyClientEvent::Exited(id) => Some(id),
                        _ => None,
                    };
                    terminal.apply(event);
                    // A shell that exits closes its tab, as in any terminal
                    // emulator; the last one returns to the chat, so `exit`
                    // always leads back even where no shortcut gets through.
                    if let Some(id) = exited {
                        terminal.remove(id);
                        let client = client.clone();
                        tokio::spawn(async move {
                            let _ = client.delete_pty(id).await;
                        });
                        if terminal.is_empty() {
                            app.terminal = None;
                            app.notice = "Terminal closed · the shell exited".into();
                        }
                    }
                }
            },
            Some(message) = side_rx.recv() => app.side_message(message),
            Some(update) = resources_rx.recv() => app.resources_arrived(update),
            Some(sent) = sends_rx.recv() => app.mentions_sent(sent, Some(&client)).await,
            Some(listed) = catalog_rx.recv() => {
                match listed {
                    Some(providers) => app.catalog_arrived(providers, &client).await,
                    None if app.models_loading => {
                        app.catalog_arrived(app.providers.clone(), &client).await;
                    }
                    None => {}
                }
            },
            Some(completion) = workspace_mutation_rx.recv() => {
                if let Some(path) = app.finish_workspace_mutation(
                    completion,
                    client.supports_local_workspace_switch(),
                ) {
                    selected_workspace = Some(path);
                    break;
                }
            },
            Some(completion) = workspace_ref_rx.recv() => {
                if let AppAction::StartWorkspaceMutation(operation) =
                    app.finish_workspace_ref_operation(completion)
                {
                    let client = client.clone();
                    let sender = workspace_mutation_tx.clone();
                    tokio::spawn(async move {
                        let completion =
                            crate::commands::run_workspace_mutation(client, operation).await;
                        let _ = sender.send(completion).await;
                    });
                }
            },
            Some(update) = statusline_rx.recv() => {
                match update {
                    statusline::Update::Output(value) => {
                        if let Some(error) = statusline_failures.recovered() {
                            tracing::info!(%error, "custom status line command recovered");
                        }
                        app.custom_status_line = Some(value);
                    }
                    statusline::Update::Error(error) => match statusline_failures.failed(&error) {
                        statusline::LogLevel::Warn => {
                            tracing::warn!(%error, "custom status line command failed");
                        }
                        statusline::LogLevel::Debug => {
                            tracing::debug!(%error, "custom status line command still failing");
                        }
                    },
                }
            },
            () = async {
                match billing.as_mut() {
                    Some(watch) => { let _ = watch.updates.changed().await; },
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let Some(watch) = &mut billing {
                    app.billing = watch.updates.borrow_and_update().clone();
                    // The summary reads live state; the actions depend on it
                    // too (sign in, turn on overage), so rebuild them in place.
                    if matches!(
                        app.overlay,
                        state::Overlay::Options { kind: state::OptionsKind::Billing, .. }
                    ) {
                        let selection = app.selection;
                        app.open_billing();
                        app.selection = selection.min(app.current_selection_length().saturating_sub(1));
                    }
                }
            },
            () = &mut shutdown => break,
            _ = tick.tick() => {
                app.tick = app.tick.wrapping_add(1);
                if follow_again_at.is_some_and(|at| std::time::Instant::now() >= at) {
                    follow_again_at = None;
                    following = Some(Restarts::default());
                    app.following_stopped = false;
                    app.connection_dropped(1);
                    (event_rx, event_task) = client.subscribe_again(Some((uuid::Uuid::nil(), 0)));
                }
                let _ = statusline_context.send(statusline::Context::from_app(&app));
                if let Err(error) = crate::input::settle_answer(&mut app, &client).await {
                    app.notice = format!("Could not answer permission: {error}");
                }
                app.expire_notification();
                app.expire_notice(std::time::Instant::now());
                app.release_completions(std::time::Instant::now());
                app.reveal_goal_completion();
                if matches!(app.overlay, state::Overlay::ProviderLogin) {
                    if let Err(error) = app.poll_login(&client).await {
                        app.notice = error.to_string();
                    }
                }
                app.poll_remote_control(&client);
                app.poll_chrome(&client);
                app.poll_logs();
                if app.goal_refresh_needed && app.tick.is_multiple_of(10) {
                    hydrate_goal_snapshot(&mut app, &client).await;
                    app.reveal_goal_completion();
                }
                if app.refresh_needed {
                    if let Ok(queue) = client.input_queue(app.thread.id).await { app.input_queue = queue; }
                    app.items = client
                        .items(app.thread.id)
                        .await
                        .unwrap_or_else(|_| app.items.clone());
                    match client.todos(app.thread.id).await {
                        Ok(todos) => app.todos = todos,
                        Err(_) => app.hydrate_todos_from_items(),
                    }
                    app.refresh_needed = false;
                }
            },
        }
    }
    if app.is_untouched_session() {
        let id = app.thread.id;
        // Leaving must stay instant; a slow daemon just keeps the row.
        commands::archive_untouched(
            &mut app.threads,
            id,
            &client,
            std::time::Duration::from_millis(300),
        )
        .await;
    }
    let draft_save = servers::save_current(&app, &client);
    event_task.abort();
    statusline_task.abort();
    client.disconnect_all_ptys();
    // Remember the last meaningful session for an explicit `--continue`.
    // A fresh, untouched home thread must not replace that resume target.
    let root_thread = app
        .parent_stack
        .first()
        .map_or(app.thread.id, |parent| parent.id);
    let offset = app.effective_viewport_offset();
    let expanded = app
        .expanded
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let meaningful = !app.items.is_empty();
    config::save_state(move |state| {
        if meaningful {
            let root_thread = root_thread.to_string();
            state.last_thread = Some(root_thread.clone());
            state.viewport_offset = Some(offset);
            state.thread_views.insert(
                root_thread,
                config::ThreadViewState {
                    viewport_offset: offset,
                    expanded,
                },
            );
        }
    });
    draft_save?;
    if quit && meaningful && client.supports_local_workspace_switch() {
        let root = app.parent_stack.first().unwrap_or(&app.thread);
        let summary = exit_summary(
            app.language,
            &root.title,
            &resume_command(root.id, app.server_info.as_ref()),
            app.active_turn.is_some(),
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        );
        drop(renderer);
        drop(terminal_guard);
        let _ = std::io::Write::write_all(&mut std::io::stdout(), summary.as_bytes());
    }
    Ok(selected_workspace)
}

/// `haarness -s <id>`, naming the folder too when the chat lives somewhere
/// other than where the shell is now (after switching folders in the TUI).
fn resume_command(
    thread: haarness_protocol::ThreadId,
    info: Option<&haarness_protocol::ServerInfo>,
) -> String {
    let here = std::env::current_dir()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    let workspace = info
        .and_then(|info| info.workspace.as_deref())
        .filter(|workspace| {
            let path = std::path::Path::new(workspace);
            here.as_deref() != Some(path.canonicalize().as_deref().unwrap_or(path))
        });
    match workspace {
        Some(workspace) => format!(
            "haarness --workspace {} -s {thread}",
            shell_quote(workspace)
        ),
        None => format!("haarness -s {thread}"),
    }
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "/._-+:@".contains(character))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// The footer left in the shell after quitting, as Upstream leaves one: which
/// chat this was and the exact command that reopens it.
fn exit_summary(
    language: crate::locale::Language,
    title: &str,
    command: &str,
    running: bool,
    styled: bool,
) -> String {
    use crate::width::DisplayWidth as _;
    use std::fmt::Write as _;
    let (dim, bold, reset) = if styled {
        ("\x1b[2m", "\x1b[1m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    let title = title.trim();
    let title: String = if title.chars().count() > 50 {
        title.chars().take(49).chain(std::iter::once('…')).collect()
    } else {
        title.to_owned()
    };
    let mut rows = Vec::new();
    if !title.is_empty() {
        rows.push((language.text("Chat"), title));
    }
    rows.push((language.text("Continue"), command.to_owned()));
    if running {
        rows.push((
            language.text("Working"),
            language
                .text("the agent keeps going on the server")
                .to_owned(),
        ));
    }
    let pad = rows
        .iter()
        .map(|(label, _)| label.width())
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = format!(
        "\n  {dim}{}{reset}\n\n",
        crate::view::home::wordmark(usize::MAX)
    );
    for (label, value) in rows {
        let gap = " ".repeat(pad - label.width());
        let _ = writeln!(out, "  {dim}{label}{gap}{reset}{bold}{value}{reset}");
    }
    out.push('\n');
    out
}

/// Pure launch selection contract, kept separate so default-new vs explicit
/// resume cannot regress behind terminal/server integration.
fn choose_initial_thread(
    threads: &[haarness_protocol::Thread],
    explicit: Option<uuid::Uuid>,
    saved: Option<uuid::Uuid>,
    continue_last: bool,
) -> Option<haarness_protocol::Thread> {
    let requested = explicit.or_else(|| continue_last.then_some(saved).flatten());
    requested
        .and_then(|id| threads.iter().find(|thread| thread.id.0 == id).cloned())
        .or_else(|| {
            if continue_last {
                threads
                    .iter()
                    .find(|thread| !thread.is_subagent && !thread.archived)
                    .cloned()
            } else {
                None
            }
        })
}
