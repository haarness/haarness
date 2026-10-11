//! Elegant server-driven terminal client rendered through OpenTUI.

pub mod api;
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
mod management;
mod markdown;
mod media;
mod messages;
mod preferences;
mod prompt_context;
mod pty_view;
mod remote_control;
mod servers;
mod state;
mod statusline;
mod terminal;
mod theme;
mod theme_embedded;
mod view;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use crossterm::event::{Event as TerminalEvent, EventStream};
use futures::StreamExt;
use haarness_opentui::{Color, ColorSupport, Renderer};
use haarness_protocol::CreateThreadRequest;
use std::{path::PathBuf, time::Duration};

use crate::api::{ApiClient, EndpointResolver};
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

async fn hydrate_initial_goal(app: &mut App, client: &ApiClient) {
    hydrate_goal_snapshot(app, client).await;
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
    let status = std::process::Command::new("sh")
        .arg("-lc")
        .arg(format!("{editor} {}", file.display()))
        .status();
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
) -> Result<()> {
    Box::pin(run_with_client(
        ApiClient::with_auth(server, auth)?.with_attachment_uploads(),
        initial_thread,
        continue_last,
        fork,
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
pub async fn run_with_thread_and_resolver_switching(
    server: &str,
    token: &str,
    initial_thread: Option<uuid::Uuid>,
    continue_last: bool,
    resolver: EndpointResolver,
) -> Result<Option<PathBuf>> {
    run_with_thread_and_resolver_fork_switching(
        server,
        token,
        initial_thread,
        continue_last,
        false,
        resolver,
    )
    .await
}

/// As [`run_with_thread_and_resolver_switching`], forking the selected or
/// continued session first so the original remains untouched.
pub async fn run_with_thread_and_resolver_fork_switching(
    server: &str,
    token: &str,
    initial_thread: Option<uuid::Uuid>,
    continue_last: bool,
    fork: bool,
    resolver: EndpointResolver,
) -> Result<Option<PathBuf>> {
    Box::pin(run_with_client(
        ApiClient::new(server, token)?.with_endpoint_resolver(resolver),
        initial_thread,
        continue_last,
        fork,
    ))
    .await
}

async fn run_with_client(
    mut client: ApiClient,
    initial_thread: Option<uuid::Uuid>,
    continue_last: bool,
    fork: bool,
) -> Result<Option<PathBuf>> {
    let initial_info = client.info().await.ok();
    let providers = client.providers().await.unwrap_or_default();
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
        }
        let thread = client.create_thread(request).await?;
        threads.push(thread.clone());
        thread
    };
    // Capture the event boundary before snapshot hydration. SSE then replays
    // exactly the deltas that raced these REST reads.
    let server_info = client.info().await.ok();
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
    let mut app = App::new(thread, threads, items, providers, agents);
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
    if !app.pending_permissions.is_empty() {
        app.permission_focus = true;
        app.update_permission_notice();
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
    let statusline_config = config::load_statusline();
    let (saved_theme, saved_variant) = config::load_theme_selection();
    app.theme_selected = saved_theme.is_some() || saved_variant.is_some();
    let initial_theme = saved_theme.unwrap_or_else(|| theme::DEFAULT_THEME.into());
    let detected_variant = crate::terminal::detect_background_variant().map(str::to_owned);
    let _terminal = TerminalGuard::enter()?;
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
    let mut input = EventStream::new();
    let initial_cursor = app
        .server_info
        .as_ref()
        .map(|info| (info.server_instance_id, info.event_cursor));
    let (mut event_rx, mut event_task) = client.subscribe(initial_cursor);
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
    let (statusline_context, mut statusline_rx, statusline_task) =
        statusline::spawn(statusline_config);
    let _ = statusline_context.send(statusline::Context::from_app(&app));
    let mut statusline_failures = statusline::FailureLog::default();
    let mut billing = haarness_account::Config::from_env()
        .and_then(haarness_account::Client::new)
        .map(haarness_account::billing::watch)
        .ok();
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
                            Ok(AppAction::Quit) => break,
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
                                tracing::warn!(error = %error, "key handling failed");
                                app.notice = "Ready".into();
                                app.show_toast(
                                    Some("Error".into()),
                                    error.to_string(),
                                    ToastVariant::Error,
                                    Duration::from_secs(7),
                                );
                            }
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
                        if app.overlay.is_filterable() {
                            if matches!(app.overlay, state::Overlay::Options { kind: state::OptionsKind::ManagementField, .. }) {
                                app.overlay_filter.push_str(&text.replace("\r\n", "\n"));
                            } else if matches!(app.overlay, state::Overlay::Options { kind: state::OptionsKind::ConnectApiKey, .. }) {
                                // A pasted key never wants the surrounding whitespace.
                                app.overlay_filter.push_str(text.trim());
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
                            Ok(AppAction::Quit) => break,
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
            Some(event) = event_rx.recv() => {
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
                        app.permission_focus = !app.pending_permissions.is_empty();
                        app.reveal_goal_completion();
                        app.preview.clear();
                        app.reasoning_preview.clear();
                        app.live_threads.remove(&app.thread.id);
                        app.notice = "Reconnected — state refreshed".into();
                    }
                    crate::api::SubscriptionEvent::ConnectionLost { attempt } => {
                        let first = app.connection_lost.is_none();
                        app.connection_lost = Some(attempt);
                        if first {
                            app.show_toast(
                                None,
                                "Connection lost · retrying".into(),
                                ToastVariant::Warning,
                                Duration::from_secs(4),
                            );
                        }
                    }
                    crate::api::SubscriptionEvent::Reconnected => {
                        app.connection_lost = None;
                        app.notice = "Reconnected".into();
                    }
                }
            },
            Ok(event) = pty_rx.recv() => {
                if let Some(terminal) = &mut app.terminal {
                    terminal.apply(event);
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
                        _ => tracing::debug!(%error, "custom status line command still failing"),
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
                let _ = statusline_context.send(statusline::Context::from_app(&app));
                app.expire_notification();
                app.release_completions(std::time::Instant::now());
                app.reveal_goal_completion();
                if matches!(app.overlay, state::Overlay::ProviderLogin) {
                    if let Err(error) = app.poll_login(&client).await {
                        app.notice = error.to_string();
                    }
                }
                app.poll_remote_control(&client);
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
    Ok(selected_workspace)
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
