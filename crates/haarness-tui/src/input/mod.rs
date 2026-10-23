//! Keyboard handling.

pub mod composer;
pub(crate) mod meta;
pub mod mouse;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use haarness_protocol::{PermissionReply, PermissionReplyDecision, QuestionReply};

use crate::api::ApiClient;
use crate::state::{App, InterruptTarget, Overlay, PermissionChoice, PhaseInfo, selected};

const CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

fn confirm<T: Copy + PartialEq>(pending: &mut Option<(T, std::time::Instant)>, target: T) -> bool {
    let now = std::time::Instant::now();
    if pending.as_ref().is_some_and(|(candidate, armed)| {
        *candidate == target && now.duration_since(*armed) <= CONFIRM_WINDOW
    }) {
        *pending = None;
        true
    } else {
        *pending = Some((target, now));
        false
    }
}

fn request_interrupt(app: &mut App, client: &ApiClient, target: InterruptTarget) {
    app.phase = Some(PhaseInfo {
        state: "stopping".into(),
        tool: None,
        call_id: None,
        since: Some(chrono::Utc::now()),
    });
    let message = if matches!(target, InterruptTarget::Session(_)) {
        "Stopping session work…"
    } else {
        "Stopping task…"
    };
    app.notice = message.into();
    let client = client.clone();
    // A running turn belongs to the thread on screen. Stopping it stops that
    // whole tree, as Desktop's Stop does, and asks the server to answer any
    // steering the agent had not read yet rather than drop it.
    let thread = app.thread.id;
    tokio::spawn(async move {
        let result = match target {
            InterruptTarget::Turn(_) => client.interrupt_session(thread, true).await,
            InterruptTarget::Session(root) => client.interrupt_session(root, true).await,
            InterruptTarget::Quit => return,
        };
        if let Err(error) = result {
            tracing::warn!(%error, "interrupt request failed");
        }
    });
}

/// Answer the first pending permission with one of the chips' keys — one
/// reply path shared by the hotkeys, the approval panel's chips, and mouse
/// clicks. The answers that reach past this one request (the persisting
/// 'a'/'w'/'r' rules, and 'u', which switches the whole chat to Auto) stage
/// a confirmation first: the panel swaps to confirm/cancel saying exactly
/// what is granted, and Enter (or the same key again) commits.
pub(crate) async fn answer_permission(
    app: &mut App,
    client: &ApiClient,
    choice: char,
) -> Result<()> {
    let Some(pending) = app.first_pending_permission() else {
        return Ok(());
    };
    if matches!(choice, 'a' | 'w' | 'r' | 'u') && pending.allow_always == Some(false) {
        return Ok(());
    }
    if choice == 'r' && pending.repository_scope.is_none() {
        return Ok(());
    }
    // Chrome site access is answered for this chat or this once: no folder
    // or repository rule, and no Auto. Allowing it in this chat covers one
    // site in one conversation, which the card already names in full, so it
    // takes a single press.
    let explicit = pending.explicit;
    if explicit && matches!(choice, 'w' | 'r' | 'u') {
        return Ok(());
    }
    let id = pending.permission_id.clone();
    let lead = crate::view::approval::staged_lead(choice, pending);
    // A legacy prompt without patterns stages too: the rule it stores is the
    // subject itself, which the card already shows.
    if matches!(choice, 'a' | 'w' | 'r' | 'u') && !explicit && app.confirm_always != Some(choice) {
        app.confirm_always = Some(choice);
        // Confirm is the panel's first chip, so a bare Enter commits.
        app.permission_selection = 0;
        // Keys first: the band is narrower than the card, which already
        // spells the grant out in full.
        app.notice = format!(
            "{} · {}",
            app.language
                .text("enter or {key} confirms, esc cancels")
                .replace("{key}", &choice.to_string()),
            app.language.text(lead)
        );
        return Ok(());
    }
    app.confirm_always = None;
    app.permission_typed = None;
    let decision = match choice {
        'y' | 'u' => PermissionReplyDecision::Once,
        'a' => PermissionReplyDecision::Always,
        'w' => PermissionReplyDecision::AlwaysWorkspace,
        'r' => PermissionReplyDecision::AlwaysRepository,
        _ => PermissionReplyDecision::Reject,
    };
    // A failed reply (typically: the server already resolved this prompt
    // through a cascade) drops the stale entry rather than erroring the
    // whole app.
    let result = client
        .reply_permission(
            &id,
            PermissionReply {
                decision,
                feedback: None,
            },
        )
        .await;
    if let Err(error) = result {
        // Keep the request visible when a reply fails; never enable Auto on failure.
        app.notice = format!("Could not answer permission: {error}");
        return Ok(());
    }
    app.auto_permissions_seen.insert(id.clone(), true);
    app.permission_answered(&id);
    if choice == 'u' {
        app.change_approval_mode(true, client).await;
    }
    Ok(())
}

/// How long a choice staged by its letter must have been on screen before the
/// same letter confirms it: long enough to read what it grants, longer than
/// the gap between the doubled letters of a typed word.
const STAGED_READ_TIME: std::time::Duration = std::time::Duration::from_millis(400);

/// A printable key while the approval card owns the keyboard. Returns false
/// when the key is not an answer, which means the user is typing a message.
///
/// `y` and `n` answer at once, and so does `a` on a Chrome site request
/// (`explicit`), whose answer for this chat covers one site. Everything else
/// a letter does here can be undone (`h`/`l` move the selection;
/// `a`/`w`/`r`/`u` only stage their answer), so those letters are
/// remembered: when the next key shows they began a word ("update…",
/// "always…", "hello"), they move to the composer with it and nothing is
/// answered. A choice staged by a letter is confirmed
/// only by that letter again once it could be read (or Enter): typed at
/// speed, "www…" or "uuid" are words, not a folder rule or Auto.
async fn permission_letter(app: &mut App, client: &ApiClient, letter: char) -> Result<bool> {
    if let Some(staged) = app.confirm_always
        && let Some((typed, _, at)) = &app.permission_typed
        && typed.ends_with(staged)
    {
        if letter != staged || at.elapsed() < STAGED_READ_TIME {
            return Ok(false);
        }
        answer_permission(app, client, letter).await?;
        return Ok(true);
    }
    let selection = app.permission_selection;
    let remember = |app: &mut App| {
        let typed = app
            .permission_typed
            .get_or_insert_with(|| (String::new(), selection, std::time::Instant::now()));
        typed.0.push(letter);
        typed.2 = std::time::Instant::now();
    };
    if matches!(letter, 'h' | 'l') {
        remember(app);
        move_permission_selection(app, if letter == 'h' { -1 } else { 1 });
        return Ok(true);
    }
    let Some(choice) = PermissionChoice::available(app)
        .into_iter()
        .find(|choice| choice.key() == letter)
    else {
        return Ok(false);
    };
    answer_permission(app, client, letter).await?;
    if choice.staged() && app.confirm_always == Some(letter) {
        remember(app);
    } else {
        app.permission_typed = None;
    }
    Ok(true)
}

/// Move the approval panel's chip selection, wrapping both directions.
fn move_permission_selection(app: &mut App, delta: isize) {
    let count = if app.confirm_always.is_some() {
        2
    } else {
        PermissionChoice::available(app).len()
    };
    let bound = isize::try_from(count).unwrap_or(1);
    let current = isize::try_from(app.permission_selection.min(count - 1)).unwrap_or(0);
    app.permission_selection = usize::try_from((current + delta).rem_euclid(bound)).unwrap_or(0);
}

#[derive(Clone, PartialEq, Eq)]
pub enum WorkspaceCreateRetry {
    /// Return to the discovered-ref picker and restore this logical row.
    Picker { value: String },
    /// Return to the arbitrary-ref prompt with the user's exact text.
    Custom { input: String },
}

#[derive(Clone, PartialEq, Eq)]
pub enum WorkspaceMutation {
    Open {
        reference: String,
    },
    Create {
        request: haarness_protocol::WorkspaceCreateRequest,
        /// Exact create surface to restore if the server rejects the request.
        retry: WorkspaceCreateRetry,
    },
    Reset {
        reference: String,
        request: haarness_protocol::WorkspaceResetRequest,
        /// Confirmation row to restore if the server rejects the mutation.
        selection: usize,
    },
    Remove {
        reference: String,
        request: haarness_protocol::WorkspaceRemoveRequest,
        /// Confirmation row to restore if the server rejects the mutation.
        selection: usize,
    },
    /// Remove only a stale registration after the server rechecks that the
    /// workspace path is still missing. Kept wire-distinct from DELETE so an
    /// older attached server fails with a safe 404.
    Cleanup {
        reference: String,
        request: haarness_protocol::WorkspaceCleanupRequest,
        /// Confirmation row to restore if the server rejects the cleanup.
        selection: usize,
    },
}

/// Short REST reads used by the workspace create wizard. They still run away
/// from the terminal input loop: ref discovery and Git resolution can be slow
/// on large repositories or remote filesystems.
#[derive(Clone, PartialEq, Eq)]
pub enum WorkspaceRefOperation {
    Load,
    Resolve { reference: String },
}

impl WorkspaceRefOperation {
    #[must_use]
    pub const fn progress_title(&self) -> &'static str {
        match self {
            Self::Load => "Loading base refs…",
            Self::Resolve { .. } => "Checking base ref…",
        }
    }
}

impl WorkspaceMutation {
    #[must_use]
    pub const fn verb(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Create { .. } => "create",
            Self::Reset { .. } => "reset",
            Self::Remove { .. } => "remove",
            Self::Cleanup { .. } => "cleanup",
        }
    }

    #[must_use]
    pub const fn progress_title(&self) -> &'static str {
        match self {
            Self::Open { .. } => "Opening folder…",
            Self::Create { .. } => "Creating worktree…",
            Self::Reset { .. } => "Resetting worktree…",
            Self::Remove { .. } => "Removing worktree…",
            Self::Cleanup { .. } => "Cleaning up folder…",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum AppAction {
    Continue,
    Quit,
    /// Restore the shell terminal and stop the foreground process group.
    Suspend,
    /// Suspend the renderer and open $EDITOR on the composer text.
    OpenEditor,
    /// Start a potentially slow Git-backed workspace operation outside the
    /// input handler so rendering, SSE, and terminal input stay live.
    StartWorkspaceMutation(WorkspaceMutation),
    /// Discover or validate a workspace base ref without blocking input/SSE.
    StartWorkspaceRefOperation(WorkspaceRefOperation),
}

pub async fn handle_key(app: &mut App, key: KeyEvent, client: &ApiClient) -> Result<AppAction> {
    // With the kitty keyboard protocol active, releases and repeats arrive as
    // distinct events; only presses (and repeats) should mutate state.
    if key.kind == KeyEventKind::Release {
        return Ok(AppAction::Continue);
    }
    app.sync_prompt_history(client);
    if app.terminal.is_some() {
        return handle_terminal_key(app, key, client).await;
    }
    // Keyboard input owns focus until the mouse actually moves again. This
    // prevents a stationary cursor from stealing selection after filtering.
    app.hovered_cell = None;
    let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
    let escape = key.code == KeyCode::Esc && !key.modifiers.contains(KeyModifiers::SHIFT);
    if !ctrl_c && !escape {
        app.escape_interrupt = None;
        app.ctrl_c_action = None;
    }
    // Reserve Shift+Esc for per-tool abort. Haarness does not yet have a
    // per-call cancellation endpoint, so it must never fall through and
    // destructively stop the whole turn.
    if key.code == KeyCode::Esc && key.modifiers.contains(KeyModifiers::SHIFT) {
        return Ok(AppAction::Continue);
    }
    if ctrl_c {
        app.escape_interrupt = None;
        if app.selection_range.is_some() {
            let selected = app.selected_text();
            if !selected.trim().is_empty() {
                crate::terminal::copy_to_clipboard(&selected);
            }
            app.selection_range = None;
            app.notice = "Copied selection".into();
        } else if app.workspace_mutation.is_some() || app.workspace_ref_operation.is_some() {
            app.notice = "Folder operation is still running · it cannot be safely cancelled".into();
        } else if matches!(app.overlay, Overlay::ProviderLogin) {
            app.cancel_login(client).await;
        } else if matches!(app.overlay, Overlay::SideChat) {
            app.side_interrupt();
        } else if !matches!(app.overlay, Overlay::None) {
            app.management = crate::management::Management::default();
            app.close_overlay();
        } else if app.cancel_queue_edit() {
            app.notice = "Edit cancelled · draft restored".into();
        } else if !app.composer.is_empty() || !app.attachments.is_empty() || app.shell_mode {
            app.composer.clear();
            app.attachments.clear();
            app.undo_draft = None;
            app.mention_suggestions.clear();
            app.shell_mode = false;
            app.autocomplete_hidden = false;
            app.selection = 0;
            app.notice = "Input cleared".into();
        } else if let Some(turn) = app.active_turn {
            let target = InterruptTarget::Turn(turn);
            if confirm(&mut app.ctrl_c_action, target) {
                request_interrupt(app, client, target);
            } else {
                app.notice = "Press Ctrl+C again to interrupt".into();
            }
        } else if app.root_descendant_active_count() > 0 {
            let target = InterruptTarget::Session(app.root_thread_id);
            if confirm(&mut app.ctrl_c_action, target) {
                request_interrupt(app, client, target);
            } else {
                app.notice = "Press Ctrl+C again to stop session work".into();
            }
        } else if confirm(&mut app.ctrl_c_action, InterruptTarget::Quit) {
            return Ok(AppAction::Quit);
        } else {
            app.notice = "Press Ctrl+C again to quit".into();
        }
        return Ok(AppAction::Continue);
    }
    // A visible dialog owns its complete key surface. Questions and
    // permissions can arrive asynchronously underneath it, but must not
    // consume arrows/Enter/Escape until the dialog closes.
    if !matches!(app.overlay, Overlay::None) {
        return handle_overlay_key(app, key, client).await;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('y') {
        app.change_approval_mode(!app.auto_permissions, client)
            .await;
        return Ok(AppAction::Continue);
    }
    // Permission keys only fire while the prompt owns focus, and only the
    // keys that answer it: anything else typed there goes to the composer, so
    // a message started while the card waits never answers it by accident.
    if app.permission_focus {
        if app.first_pending_permission().is_some() {
            let plain = key.modifiers.is_empty();
            if let KeyCode::Char(letter) = key.code
                && (plain || key.modifiers == KeyModifiers::SHIFT)
            {
                if plain && permission_letter(app, client, letter).await? {
                    return Ok(AppAction::Continue);
                }
                // A child thread has no composer to type into.
                if app.is_child_view() {
                    app.permission_typed = None;
                    app.confirm_always = None;
                    app.update_permission_notice();
                    return Ok(AppAction::Continue);
                }
                app.release_permission_focus(true);
            } else {
                // Any other key is a deliberate move on the card.
                app.permission_typed = None;
                match key.code {
                    KeyCode::Char('g') if key.modifiers == KeyModifiers::CONTROL => {
                        // The keyboard goes back to the prompt; the card waits.
                        app.release_permission_focus(false);
                        return Ok(AppAction::Continue);
                    }
                    KeyCode::Left if plain => {
                        move_permission_selection(app, -1);
                        return Ok(AppAction::Continue);
                    }
                    KeyCode::Right | KeyCode::Tab if plain => {
                        move_permission_selection(app, 1);
                        return Ok(AppAction::Continue);
                    }
                    KeyCode::BackTab => {
                        move_permission_selection(app, -1);
                        return Ok(AppAction::Continue);
                    }
                    // Reading the rest of a long command keeps a staged
                    // answer staged: that is when it is worth re-reading.
                    KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown if plain => {
                        let page = app.permission_fit.get().shown.max(1).cast_signed();
                        app.scroll_permission_subject(match key.code {
                            KeyCode::Up => -1,
                            KeyCode::Down => 1,
                            KeyCode::PageUp => -page,
                            _ => page,
                        });
                        return Ok(AppAction::Continue);
                    }
                    KeyCode::Char('e') if key.modifiers == KeyModifiers::CONTROL => {
                        app.toggle_permission_subject();
                        return Ok(AppAction::Continue);
                    }
                    KeyCode::Enter if plain => {
                        if let Some(staged) = app.confirm_always {
                            if app.permission_selection == 0 {
                                answer_permission(app, client, staged).await?;
                            } else {
                                app.confirm_always = None;
                                app.permission_selection = 0;
                                app.update_permission_notice();
                            }
                        } else {
                            let choices = PermissionChoice::available(app);
                            let choice = choices[app.permission_selection.min(choices.len() - 1)];
                            answer_permission(app, client, choice.key()).await?;
                        }
                        return Ok(AppAction::Continue);
                    }
                    KeyCode::Esc => {
                        if app.confirm_always.take().is_some() {
                            app.permission_selection = 0;
                            app.update_permission_notice();
                        } else if let Some(pending) = app.first_pending_permission() {
                            let id = pending.permission_id.clone();
                            let _ = client
                                .reply_permission(
                                    &id,
                                    PermissionReply {
                                        decision: PermissionReplyDecision::Reject,
                                        feedback: None,
                                    },
                                )
                                .await;
                            app.permission_answered(&id);
                            app.notice = "Permission rejected".into();
                        }
                        return Ok(AppAction::Continue);
                    }
                    _ => {
                        // Swallow everything else so a stray key neither
                        // answers the prompt nor edits a prompt it hides.
                        app.confirm_always = None;
                        app.update_permission_notice();
                        return Ok(AppAction::Continue);
                    }
                }
            }
        }
        app.permission_focus = false;
    }
    // A child-agent thread is a read-only inspector. Enforce that here, ahead
    // of question/composer routing and every editor binding, so hiding the
    // prompt cannot leave a mutable or submittable draft behind. Approvals
    // above remain actionable because they control the running task tree,
    // while ordinary text input must be answered from the parent session.
    if app.is_child_view() {
        if crate::keybindings::resolve(&app.keybindings, key) == Some("messages") {
            app.open_messages(None);
            return Ok(AppAction::Continue);
        }
        app.composer.clear();
        app.attachments.clear();
        app.mention_suggestions.clear();
        app.shell_mode = false;
        app.autocomplete_hidden = false;
        app.selection = 0;
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('d')) => return Ok(AppAction::Quit),
            (KeyModifiers::CONTROL, KeyCode::Char('z' | 'Z')) => {
                return Ok(AppAction::Suspend);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                app.refresh_threads(client).await?;
                app.open_overlay(Overlay::Threads);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('o')) => app.open_tasks_overlay(),
            (KeyModifiers::CONTROL, KeyCode::Char('b')) => {
                app.toggle_sidebar();
                if app.sidebar_open {
                    app.mcp_status = client.mcp().await.unwrap_or_default();
                }
            }
            (KeyModifiers::CONTROL, KeyCode::Char('t')) => app.cycle_thinking(),
            (KeyModifiers::CONTROL, KeyCode::Char('g')) => {
                if app.pending_permissions.is_empty() {
                    app.notice = "No approvals pending".into();
                } else {
                    app.permission_focus = true;
                    app.update_permission_notice();
                }
            }
            (KeyModifiers::NONE, KeyCode::Up) => app.leave_child(client).await?,
            (KeyModifiers::NONE, KeyCode::Left) => app.cycle_child(-1, client).await?,
            (KeyModifiers::NONE, KeyCode::Right) => app.cycle_child(1, client).await?,
            (KeyModifiers::NONE, KeyCode::PageUp) => app.scroll(app.page_rows()),
            (KeyModifiers::NONE, KeyCode::PageDown) => app.scroll(-app.page_rows()),
            (KeyModifiers::NONE | KeyModifiers::CONTROL, KeyCode::Home) => app.scroll(1_000_000),
            (KeyModifiers::NONE | KeyModifiers::CONTROL, KeyCode::End) => app.scroll_to_bottom(),
            _ => {}
        }
        return Ok(AppAction::Continue);
    }
    if let Some(question) = app.first_pending_question().cloned() {
        let id = question.question_id;
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        // `custom` is an explicit protocol contract, independent of whether
        // the question is single- or multi-select. In particular, a
        // single-select question with `custom: false` must never turn the
        // composer (including a draft that predates the prompt) into an
        // unadvertised free-form answer.
        let custom_allowed = question.custom;
        match key.code {
            KeyCode::Esc | KeyCode::Char('d')
                if key.code == KeyCode::Esc || key.modifiers == KeyModifiers::CONTROL =>
            {
                // The protocol's question channel is textual; resolve it with
                // an explicit cancellation answer so the waiting tool does not
                // hang.
                client
                    .reply_question(
                        &id,
                        QuestionReply {
                            answer: "Cancelled by user".into(),
                        },
                    )
                    .await?;
                app.question_answered(&id);
                app.composer.clear();
                app.notice = "Question cancelled".into();
                return Ok(AppAction::Continue);
            }
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if custom_allowed {
                    app.composer.insert_char('\n');
                }
                return Ok(AppAction::Continue);
            }
            KeyCode::Enter if plain && question.multiple => {
                let mut answers = app
                    .question_selected
                    .iter()
                    .filter_map(|index| question.choices.get(*index).cloned())
                    .collect::<Vec<_>>();
                if question.custom && !app.composer.text().trim().is_empty() {
                    let custom = app.composer.take();
                    app.save_prompt_history();
                    answers.extend(
                        custom
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(str::to_owned),
                    );
                }
                let answer = serde_json::to_string(&answers)?;
                client.reply_question(&id, QuestionReply { answer }).await?;
                app.question_answered(&id);
                return Ok(AppAction::Continue);
            }
            KeyCode::Enter if plain => {
                let answer = if custom_allowed && !app.composer.text().trim().is_empty() {
                    let answer = app.composer.take();
                    app.save_prompt_history();
                    Some(answer)
                } else {
                    app.question_choice(app.question_selection)
                };
                if let Some(answer) = answer {
                    client.reply_question(&id, QuestionReply { answer }).await?;
                    app.question_answered(&id);
                }
                return Ok(AppAction::Continue);
            }
            KeyCode::Up
                if (!custom_allowed || app.composer.is_empty()) && !question.choices.is_empty() =>
            {
                app.move_question_selection(-1);
                return Ok(AppAction::Continue);
            }
            KeyCode::Down
                if (!custom_allowed || app.composer.is_empty()) && !question.choices.is_empty() =>
            {
                app.move_question_selection(1);
                return Ok(AppAction::Continue);
            }
            KeyCode::Char(' ')
                if plain && question.multiple && (!custom_allowed || app.composer.is_empty()) =>
            {
                app.toggle_question_choice(app.question_selection);
                return Ok(AppAction::Continue);
            }
            // Digit quick-answers when the question offered choices.
            KeyCode::Char(digit @ '1'..='9') if !custom_allowed || app.composer.is_empty() => {
                let index = digit as usize - '1' as usize;
                if question.multiple {
                    app.question_selection = index.min(question.choices.len().saturating_sub(1));
                    app.toggle_question_choice(index);
                    return Ok(AppAction::Continue);
                }
                if let Some(choice) = app.question_choice(index) {
                    client
                        .reply_question(&id, QuestionReply { answer: choice })
                        .await?;
                    app.question_answered(&id);
                    return Ok(AppAction::Continue);
                }
            }
            // Once the user types, the composer is an explicit custom-answer
            // editor. It owns editing keys, never history, agent cycling, or
            // global dialogs. The selected choice also stops rendering as
            // active so Enter cannot visually promise a different answer.
            KeyCode::Char(character) if plain && custom_allowed => {
                app.composer.insert_char(character);
            }
            KeyCode::Char(_) if plain => return Ok(AppAction::Continue),
            // With custom answers disabled, swallow every remaining editing
            // binding. Choice navigation/clicks still work, while a draft
            // that existed before the asynchronous question remains intact.
            _ if !custom_allowed => return Ok(AppAction::Continue),
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.composer.delete_word_back();
            }
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {
                app.composer.delete_word_back();
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                app.composer.move_word_left();
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                app.composer.move_word_right();
            }
            KeyCode::Backspace => app.composer.backspace(),
            KeyCode::Delete => app.composer.delete(),
            KeyCode::Left => app.composer.move_left(),
            KeyCode::Right => app.composer.move_right(),
            KeyCode::Home => app.composer.move_home(),
            KeyCode::End => app.composer.move_end(),
            KeyCode::Up => {
                app.composer.move_up();
            }
            KeyCode::Down => {
                app.composer.move_down();
            }
            KeyCode::PageUp if key.modifiers.is_empty() => app.scroll(app.page_rows()),
            KeyCode::PageDown if key.modifiers.is_empty() => app.scroll(-app.page_rows()),
            KeyCode::Char('z' | 'Z') if key.modifiers == KeyModifiers::SUPER => {
                app.composer.undo();
            }
            KeyCode::Char('z' | 'Z')
                if key.modifiers == KeyModifiers::SUPER | KeyModifiers::SHIFT =>
            {
                app.composer.redo();
            }
            KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match character {
                    'a' => app.composer.move_home(),
                    'e' => app.composer.move_end(),
                    'f' => app.composer.move_right(),
                    'w' => app.composer.delete_word_back(),
                    'k' => app.composer.kill_to_end(),
                    'u' => app.composer.kill_to_start(),
                    '-' => app.composer.undo(),
                    '.' => app.composer.redo(),
                    _ => {}
                }
            }
            _ => {}
        }
        return Ok(AppAction::Continue);
    }
    // Haarness treats Ctrl+D as an immediate app exit only from an empty
    // prompt. Dialogs, approvals, and questions have already had first claim
    // on the key above; a non-empty composer keeps readline delete-forward.
    if key.modifiers == KeyModifiers::CONTROL
        && key.code == KeyCode::Char('d')
        && app.composer.is_empty()
        && app.attachments.is_empty()
        && !app.shell_mode
    {
        return Ok(AppAction::Quit);
    }
    // Terminal and composer history bindings use exact modifiers. Keep
    // these after prompt/dialog routing so an overlay continues to own its
    // keystrokes, and require exact modifiers so Ctrl+Shift+Z cannot stop a
    // running job by accident.
    if matches!(key.code, KeyCode::Char('z' | 'Z')) {
        if key.modifiers == KeyModifiers::CONTROL {
            return Ok(AppAction::Suspend);
        }
        if key.modifiers == KeyModifiers::SUPER {
            app.composer.undo();
            return Ok(AppAction::Continue);
        }
        if key.modifiers == KeyModifiers::SUPER | KeyModifiers::SHIFT {
            app.composer.redo();
            return Ok(AppAction::Continue);
        }
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('p' | 'n'))
        && (app.slash_suggestions().is_some() || !app.mention_suggestions.is_empty())
    {
        app.move_selection(if key.code == KeyCode::Char('p') {
            -1
        } else {
            1
        });
        return Ok(AppAction::Continue);
    }
    if let Some(action) = crate::keybindings::resolve(&app.keybindings, key).map(str::to_owned) {
        match action.as_str() {
            "commands" => app.open_overlay(Overlay::Commands),
            "stash_save" => {
                app.open_management("stash", &serde_json::json!({"action":"save"}), client)
                    .await?;
                app.close_overlay();
                app.notice = "Draft saved to stash".into();
            }
            action => return app.execute_command(&format!("/{action}"), client).await,
        }
        return Ok(AppAction::Continue);
    }
    if crate::keybindings::overrides_default(&app.keybindings, key) {
        return Ok(AppAction::Continue);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('p') => app.open_overlay(Overlay::Commands),
            KeyCode::Char('l') => {
                app.refresh_threads(client).await?;
                app.open_overlay(Overlay::Threads);
            }
            KeyCode::Char('g') => {
                if app.pending_permissions.is_empty() {
                    app.notice = "No approvals pending".into();
                } else {
                    app.permission_focus = true;
                    app.update_permission_notice();
                }
            }
            KeyCode::Char('o') => {
                app.open_tasks_overlay();
            }
            KeyCode::Char('b') => {
                app.toggle_sidebar();
                if app.sidebar_open {
                    app.mcp_status = client.mcp().await.unwrap_or_default();
                }
            }
            KeyCode::Char('t') => app.cycle_thinking(),
            KeyCode::Char('a') => app.composer.move_home(),
            KeyCode::Char('e') => app.composer.move_end(),
            KeyCode::Char('f') => app.composer.move_right(),
            KeyCode::Char('d') => app.composer.delete(),
            KeyCode::Char('w') | KeyCode::Backspace => app.composer.delete_word_back(),
            KeyCode::Char('k') => app.composer.kill_to_end(),
            KeyCode::Char('u') => app.composer.kill_to_start(),
            KeyCode::Char('-') if key.modifiers == KeyModifiers::CONTROL => app.composer.undo(),
            KeyCode::Char('.') if key.modifiers == KeyModifiers::CONTROL => app.composer.redo(),
            KeyCode::Char('j') | KeyCode::Enter => app.composer.insert_char('\n'),
            KeyCode::Left => app.composer.move_word_left(),
            KeyCode::Right => app.composer.move_word_right(),
            KeyCode::Delete => app.composer.delete_word_forward(),
            KeyCode::Home => app.scroll(1_000_000),
            KeyCode::End => app.scroll_to_bottom(),
            _ => {}
        }
        app.selection = 0;
        app.autocomplete_hidden = false;
        return Ok(AppAction::Continue);
    }
    if key.modifiers.contains(KeyModifiers::ALT) && key.code != KeyCode::Enter {
        match key.code {
            KeyCode::Char('b') | KeyCode::Left => app.composer.move_word_left(),
            KeyCode::Char('f') | KeyCode::Right => app.composer.move_word_right(),
            KeyCode::Char('d') => app.composer.delete_word_forward(),
            KeyCode::Backspace => app.composer.delete_word_back(),
            KeyCode::Enter => app.composer.insert_char('\n'),
            _ => {}
        }
        app.selection = 0;
        return Ok(AppAction::Continue);
    }
    match key.code {
        KeyCode::Esc => {
            if app.selection_range.is_some() {
                app.selection_range = None;
            } else if !app.mention_suggestions.is_empty() || app.slash_suggestions().is_some() {
                app.mention_suggestions.clear();
                app.autocomplete_hidden = true;
                app.selection = 0;
            } else if app.shell_mode {
                app.shell_mode = false;
                app.composer.clear();
                app.notice = "Ready".into();
            } else if app.cancel_queue_edit() {
                // While a queued message is open, Esc belongs to the edit. It
                // must never reach running work, not even as a second press.
                app.escape_interrupt = None;
                app.notice = "Edit cancelled · draft restored".into();
            } else if let Some(turn) = app.active_turn {
                app.ctrl_c_action = None;
                let target = InterruptTarget::Turn(turn);
                if confirm(&mut app.escape_interrupt, target) {
                    request_interrupt(app, client, target);
                } else {
                    app.notice = "Press Esc again to interrupt".into();
                }
            } else if app.root_descendant_active_count() > 0 {
                app.ctrl_c_action = None;
                let target = InterruptTarget::Session(app.root_thread_id);
                if confirm(&mut app.escape_interrupt, target) {
                    request_interrupt(app, client, target);
                } else {
                    app.notice = "Press Esc again to stop session work".into();
                }
            } else {
                app.escape_interrupt = None;
            }
        }
        KeyCode::Up => {
            if app.slash_suggestions().is_some() || !app.mention_suggestions.is_empty() {
                app.move_selection(-1);
            } else if !app.composer.move_up() && app.composer.history_prev() {
                app.selection = 0;
            }
        }
        KeyCode::Down => {
            if app.slash_suggestions().is_some() || !app.mention_suggestions.is_empty() {
                app.move_selection(1);
            } else if !app.composer.move_down() {
                app.composer.history_next();
            }
        }
        KeyCode::Left => app.composer.move_left(),
        KeyCode::Right => app.composer.move_right(),
        KeyCode::Home => {
            if app.composer.is_empty() && app.effective_viewport_offset() > 0 {
                app.scroll(1_000_000);
            } else {
                app.composer.move_home();
            }
        }
        KeyCode::End => {
            if app.effective_viewport_offset() > 0 {
                app.scroll_to_bottom();
            } else {
                app.composer.move_end();
            }
        }
        KeyCode::PageUp => app.scroll(app.page_rows()),
        KeyCode::PageDown => app.scroll(-app.page_rows()),
        KeyCode::Delete => {
            app.composer.delete();
            app.autocomplete_hidden = false;
            app.selection = 0;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            if crate::prompt_context::apply(app, client).await? {
                // A directory selection immediately opens its next level;
                // completed files have no active query and simply clear.
                refresh_mentions(app, client).await;
            } else if let Some(commands) = app.slash_suggestions() {
                if let Some(entry) = selected(&commands, app.selection) {
                    let name = entry.name.clone();
                    app.composer.set_text(format!("/{name} "));
                }
            } else if key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT) {
                app.cycle_agent_reverse();
            } else {
                app.cycle_agent();
            }
        }
        KeyCode::Backspace => {
            if app.shell_mode && app.composer.is_empty() {
                app.shell_mode = false;
                app.notice = "Ready".into();
            } else if app.composer.is_empty() && !app.attachments.is_empty() {
                if let Some(path) = app.attachments.pop() {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into());
                    app.notice = format!("Removed {name}");
                }
            } else {
                app.composer.backspace();
                // Erasing a slash command typed over an undone prompt
                // brings the prompt back.
                app.return_undo_draft();
                app.autocomplete_hidden = false;
                app.selection = 0;
                refresh_mentions(app, client).await;
            }
        }
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                app.composer.insert_char('\n');
            } else if crate::prompt_context::apply(app, client).await? {
                refresh_mentions(app, client).await;
            } else if app.queue_edit.is_some() {
                // Saving replaces the queued message in place. Alt+Enter
                // would queue anyway, so both keys save.
                app.scroll_to_bottom();
                app.save_queue_edit(client).await?;
            } else if key.modifiers.contains(KeyModifiers::ALT)
                && (app.shell_mode || app.composer.text().trim_start().starts_with('/'))
            {
                app.notice = "Commands cannot be queued. Use a normal message or Steer.".into();
            } else if app.shell_mode {
                let command = app.composer.take();
                app.save_prompt_history();
                if !command.trim().is_empty() {
                    app.notice = "Shell command started…".into();
                    match client.run_shell(app.thread.id, &command).await {
                        Ok(()) => app.shell_mode = false,
                        Err(error) => {
                            app.composer.set_text(command);
                            app.show_toast(
                                Some("Shell failed".into()),
                                error.to_string(),
                                crate::state::ToastVariant::Error,
                                std::time::Duration::from_secs(8),
                            );
                        }
                    }
                    app.refresh_needed = true;
                    app.scroll_to_bottom();
                }
            } else if let Some(raw) = selected_slash_command(app) {
                app.composer.clear();
                app.selection = 0;
                let action = app.execute_command(&raw, client).await;
                app.return_undo_draft();
                return action;
            } else if let Some(name) = unknown_slash_command(app) {
                // A mistyped command must not reach the model as a prompt.
                // The draft stays so the typo can be fixed in place. A running
                // turn owns the status line, so say it on a card then.
                let notice = app.unknown_command_notice(&name);
                if app.active_turn.is_some() {
                    app.show_toast(
                        None,
                        notice.clone(),
                        crate::state::ToastVariant::Warning,
                        std::time::Duration::from_secs(5),
                    );
                }
                app.notice = notice;
            } else if !app.composer.text().trim().is_empty() || !app.attachments.is_empty() {
                // Resolve before taking the draft; a failed settings read must
                // preserve text and attachments for retry.
                let delivery = if key.modifiers.contains(KeyModifiers::ALT) {
                    haarness_protocol::InputDelivery::Queue
                } else {
                    client.input_delivery().await?
                };
                let text = app.composer.take();
                app.scroll_to_bottom();
                let sent = if delivery == haarness_protocol::InputDelivery::Queue {
                    app.defer_prompt(text.clone(), client).await
                } else if app.active_turn.is_some() {
                    // Mid-turn input queues and steers into the running turn.
                    app.queue_prompt(text.clone(), client).await
                } else {
                    app.submit_prompt(text.clone(), client).await
                };
                match sent {
                    Ok(true) => {
                        app.undo_draft = None;
                        app.save_prompt_history();
                    }
                    Ok(false) => app.composer.set_text(text),
                    Err(error) => {
                        app.composer.set_text(text);
                        return Err(error);
                    }
                }
            }
        }
        KeyCode::Char('!')
            if app.composer.is_empty() && !app.shell_mode && app.queue_edit.is_none() =>
        {
            app.shell_mode = true;
            app.notice = "shell — Enter runs the command, Esc leaves".into();
        }
        KeyCode::Char(character) => {
            // `/` over an untouched undone prompt starts a command (such as
            // the /redo the undo notice offers) rather than appending to it.
            // With the cursor moved into the prompt, it is part of an edit
            // (`src/main.rs`).
            if character == '/'
                && app.composer.cursor() == app.composer.text().len()
                && app.holds_undo_draft()
            {
                app.undo_draft = Some(crate::state::UndoDraft {
                    text: app.composer.expanded_text(),
                    attachments: std::mem::take(&mut app.attachments),
                });
                app.composer.clear();
            }
            app.composer.insert_char(character);
            app.autocomplete_hidden = false;
            app.selection = 0;
            refresh_mentions(app, client).await;
        }
        _ => {}
    }
    Ok(AppAction::Continue)
}

async fn handle_terminal_key(
    app: &mut App,
    key: KeyEvent,
    client: &ApiClient,
) -> Result<AppAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if crate::pty_view::is_detach_key(key) {
        if let Some(terminal) = app.terminal.take() {
            for id in terminal.ids() {
                client.disconnect_pty(id);
            }
        }
        app.notice = "Terminal detached · processes are still running".into();
        return Ok(AppAction::Continue);
    }
    if ctrl
        && shift
        && matches!(key.code, KeyCode::Char(character) if character.eq_ignore_ascii_case(&'t'))
    {
        let info = client
            .create_pty(&haarness_protocol::PtyCreateRequest::default())
            .await?;
        client.connect_pty(info.id, 0);
        if let Some(terminal) = &mut app.terminal {
            terminal.push(info);
        }
        return Ok(AppAction::Continue);
    }
    if ctrl
        && shift
        && matches!(key.code, KeyCode::Char(character) if character.eq_ignore_ascii_case(&'w'))
    {
        let id = app
            .terminal
            .as_ref()
            .and_then(crate::pty_view::TerminalWorkspace::active_id);
        if let Some(id) = id {
            client.delete_pty(id).await?;
            if let Some(terminal) = &mut app.terminal {
                terminal.remove(id);
                if terminal.is_empty() {
                    app.terminal = None;
                    app.notice = "Terminal closed".into();
                }
            }
        }
        return Ok(AppAction::Continue);
    }
    if alt && matches!(key.code, KeyCode::Left | KeyCode::Right) {
        if let Some(terminal) = &mut app.terminal {
            terminal.next(if key.code == KeyCode::Left { -1 } else { 1 });
        }
        return Ok(AppAction::Continue);
    }
    if shift && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
        if let Some(terminal) = &mut app.terminal {
            let page = terminal.page_rows();
            terminal.scroll(if key.code == KeyCode::PageUp {
                -page
            } else {
                page
            });
        }
        return Ok(AppAction::Continue);
    }
    if let Some((id, bytes)) = app
        .terminal
        .as_ref()
        .and_then(|terminal| Some((terminal.active_id()?, terminal.input_bytes(key)?)))
    {
        if let Some(terminal) = &mut app.terminal {
            terminal.follow_output();
        }
        client.write_pty(id, bytes).await?;
    }
    Ok(AppAction::Continue)
}

/// Resolve Enter on a slash composer. A partial query activates the selected
/// suggestion; a command with arguments executes only when its name is exact.
/// Anything else is [`unknown_slash_command`] or ordinary prompt text.
pub(crate) fn selected_slash_command(app: &App) -> Option<String> {
    if !app.composer.text().starts_with('/') {
        return None;
    }
    // An exact command or alias always outranks fuzzy description matches.
    // For example, `/workflow` also matches `/ultracode`'s description; using
    // the stale first suggestion there would execute the wrong command.
    if app.is_known_command(app.composer.text()) {
        return Some(app.composer.text().to_owned());
    }
    if let Some(commands) = app.slash_suggestions() {
        return selected(&commands, app.selection).map(|entry| format!("/{}", entry.name));
    }
    None
}

/// The name a slash draft calls when no command answers to it. Paths and
/// other prose (`/usr/bin is…`) still go to the model as text, and so does a
/// draft that starts with a space.
pub(crate) fn unknown_slash_command(app: &App) -> Option<String> {
    let text = app.composer.text();
    let name = text.strip_prefix('/')?.split(char::is_whitespace).next()?;
    let command_like = name.starts_with(|character: char| character.is_ascii_alphabetic())
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':')
        });
    (command_like && !app.is_known_command(text)).then(|| name.to_owned())
}

/// Complete the selected @-mention, when suggestions are showing.
pub(crate) fn apply_mention(app: &mut App) -> bool {
    if app.mention_suggestions.is_empty() {
        return false;
    }
    let Some((start, query)) = app.composer.mention_query() else {
        app.mention_suggestions.clear();
        return false;
    };
    let Some(path) = selected(&app.mention_suggestions, app.selection).cloned() else {
        return false;
    };
    let end = start + 1 + query.len();
    let absolute = if let Some(rest) = path.strip_prefix("~/") {
        haarness_protocol::home_dir().map(|home| home.join(rest))
    } else if std::path::Path::new(&path).is_absolute() {
        Some(std::path::PathBuf::from(&path))
    } else {
        app.server_info
            .as_ref()
            .and_then(|info| info.workspace.as_deref())
            .map(|workspace| std::path::Path::new(workspace).join(&path))
    };
    if absolute.as_ref().is_some_and(|path| path.is_dir()) {
        app.composer.replace_range(start, end, &format!("@{path}"));
        app.mention_suggestions.clear();
        app.selection = 0;
        return true;
    }
    if let Some(absolute) = absolute
        && absolute.is_file()
        && crate::media::mime_for_path(&absolute).is_some()
        && let Ok(canonical) = absolute.canonicalize()
    {
        app.composer.replace_range(start, end, "");
        let added = app.add_attachments(vec![canonical.display().to_string()]);
        app.mention_suggestions.clear();
        app.selection = 0;
        if added > 0 {
            app.notice = format!("Attached {path}");
        }
        return true;
    }
    app.composer.replace_range(start, end, &format!("@{path} "));
    app.mention_suggestions.clear();
    app.selection = 0;
    true
}

/// Query the server for @-mention completions matching the token under the
/// cursor. Errors and missing endpoints degrade to no suggestions.
pub(crate) async fn refresh_mentions(app: &mut App, client: &ApiClient) {
    if app.shell_mode || app.composer.text().starts_with('/') {
        app.mention_suggestions.clear();
        return;
    }
    match app.composer.mention_query() {
        Some((_, query)) => {
            let (path, range) = crate::prompt_context::range(&query);
            let mut suggestions = app
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
            let files = client.search_files(path, 8).await.unwrap_or_default();
            suggestions.extend(files.into_iter().map(|file| {
                if let Some((start, end)) = range {
                    format!("{file}:{start}-{end}")
                } else {
                    file
                }
            }));
            app.mention_suggestions = suggestions;
            app.selection = 0;
        }
        None => app.mention_suggestions.clear(),
    }
}

/// Keys inside the full-screen review. Returns false when the viewer did not
/// claim the key, so the generic report handling still applies.
async fn handle_diff_key(app: &mut App, key: KeyEvent, client: &ApiClient) -> Result<bool> {
    use crate::diffview::{CONTEXT_STEPS, DiffFocus, DiffLayout};

    let Overlay::Diff(viewer) = &mut app.overlay else {
        return Ok(false);
    };

    // A comment is a text field: everything typable belongs to it until the
    // reader commits or cancels.
    if let Some(draft) = &mut viewer.comment_input {
        match key.code {
            KeyCode::Esc => viewer.comment_input = None,
            KeyCode::Enter => {
                let text = std::mem::take(draft);
                viewer.comment_input = None;
                if let Some((file, line)) = viewer.comment_target() {
                    viewer.upsert_comment(file, line, &text);
                }
            }
            KeyCode::Backspace => {
                draft.pop();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                draft.push(character);
            }
            _ => {}
        }
        return Ok(true);
    }

    if viewer.help {
        // The help list is modal over the viewer: any key returns to it.
        viewer.help = false;
        return Ok(true);
    }

    let tree = viewer.focus == DiffFocus::Tree;
    let page = isize::try_from(viewer.viewport.get().max(1)).unwrap_or(1);
    let mut reload = false;
    let mut close = false;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => close = true,
        KeyCode::Tab | KeyCode::BackTab => {
            if viewer.tree_showing() {
                viewer.focus = if tree {
                    DiffFocus::Patch
                } else {
                    DiffFocus::Tree
                };
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if tree {
                viewer.move_tree_selection(1);
            } else {
                viewer.move_cursor(true);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if tree {
                viewer.move_tree_selection(-1);
            } else {
                viewer.move_cursor(false);
            }
        }
        // Paging is Page keys plus the vim control chords; plain letters stay
        // free for the review's own single-key actions.
        KeyCode::PageDown if !tree => viewer.scroll_by(page),
        KeyCode::PageUp if !tree => viewer.scroll_by(-page),
        KeyCode::Char('f') if !tree && key.modifiers.contains(KeyModifiers::CONTROL) => {
            viewer.scroll_by(page);
        }
        KeyCode::Char('b') if !tree && key.modifiers.contains(KeyModifiers::CONTROL) => {
            viewer.scroll_by(-page);
        }
        KeyCode::Char('d') if !tree && key.modifiers.contains(KeyModifiers::CONTROL) => {
            viewer.scroll_by(page / 2);
        }
        KeyCode::Char('u') if !tree && key.modifiers.contains(KeyModifiers::CONTROL) => {
            viewer.scroll_by(-page / 2);
        }
        KeyCode::PageDown => viewer.move_tree_selection(8),
        KeyCode::PageUp => viewer.move_tree_selection(-8),
        KeyCode::Home | KeyCode::Char('g') => {
            if tree {
                viewer.move_tree_selection(isize::MIN / 2);
            } else {
                viewer.scroll = 0;
                viewer.cursor = None;
                viewer.move_cursor(true);
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            if tree {
                viewer.move_tree_selection(isize::MAX / 2);
            } else {
                viewer.scroll = viewer.max_scroll();
                viewer.cursor = Some(viewer.display.len().saturating_sub(1));
                if !viewer
                    .display
                    .last()
                    .is_some_and(crate::diffview::DisplayRow::selectable)
                {
                    viewer.move_cursor(false);
                }
            }
        }
        KeyCode::Enter => {
            if tree {
                viewer.activate_tree_row();
            }
        }
        KeyCode::Right | KeyCode::Char('l') if tree => viewer.expand_tree_row(),
        KeyCode::Left | KeyCode::Char('h') if tree => viewer.collapse_tree_row(),
        KeyCode::Char('a') if tree => viewer.expand_all(),
        KeyCode::Char('n') => viewer.jump_change(true),
        KeyCode::Char('p') => viewer.jump_change(false),
        KeyCode::Char(']') => viewer.jump_file(true),
        KeyCode::Char('[') => viewer.jump_file(false),
        KeyCode::Char('t') => {
            viewer.tree_visible = !viewer.tree_visible;
            if !viewer.tree_visible {
                viewer.focus = DiffFocus::Patch;
            }
        }
        KeyCode::Char('o') => {
            viewer.single_file = !viewer.single_file;
            let active = viewer.active_file;
            viewer.rebuild_display();
            viewer.focus_file(active);
        }
        KeyCode::Char('v') => {
            viewer.layout = Some(match viewer.layout_in_use() {
                DiffLayout::Split => DiffLayout::Unified,
                DiffLayout::Unified => DiffLayout::Split,
            });
            let active = viewer.active_file;
            viewer.rebuild_display();
            viewer.focus_file(active);
        }
        KeyCode::Char('x') => {
            viewer.context_step = (viewer.context_step + 1) % CONTEXT_STEPS.len();
            viewer.loading = true;
            reload = true;
        }
        KeyCode::Char('r') => {
            viewer.toggle_reviewed();
        }
        KeyCode::Char('c') => {
            viewer.comment_input = Some(
                viewer
                    .comment_target()
                    .and_then(|(file, line)| viewer.existing_comment(&file, line).cloned())
                    .map(|comment| comment.text)
                    .unwrap_or_default(),
            )
        }
        KeyCode::Char('s') => {
            viewer.source = match viewer.source {
                crate::diffview::DiffSource::WorkingTree => crate::diffview::DiffSource::Branch,
                crate::diffview::DiffSource::Branch => crate::diffview::DiffSource::LastTurn,
                crate::diffview::DiffSource::LastTurn => crate::diffview::DiffSource::WorkingTree,
            };
            viewer.loading = true;
            reload = true;
        }
        KeyCode::Char('?') => viewer.help = true,
        _ => return Ok(false),
    }
    if close {
        app.close_diff_viewer();
    } else if reload {
        app.reload_diff_viewer(client).await;
    }
    Ok(true)
}

async fn handle_overlay_key(app: &mut App, key: KeyEvent, client: &ApiClient) -> Result<AppAction> {
    if matches!(app.overlay, Overlay::Models)
        && key.code == KeyCode::Char('f')
        && key.modifiers == KeyModifiers::CONTROL
    {
        app.toggle_model_favorite();
        return Ok(AppAction::Continue);
    }
    if key.code == KeyCode::Esc {
        if app.management.details
            || matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::ManagementField
                        | crate::state::OptionsKind::ManagementValue
                        | crate::state::OptionsKind::ManagementConfirm,
                    ..
                }
            )
        {
            app.show_management();
            return Ok(AppAction::Continue);
        }
        if matches!(
            app.overlay,
            Overlay::Options {
                kind: crate::state::OptionsKind::Management,
                ..
            }
        ) {
            app.management = crate::management::Management::default();
        }
    }
    // The key prompt's inline reason belongs to the key it was about; any
    // edit makes it stale.
    if key.code != KeyCode::Enter
        && matches!(
            app.overlay,
            Overlay::Options {
                kind: crate::state::OptionsKind::ConnectApiKey,
                ..
            }
        )
    {
        app.connect.key_error = None;
    }
    // Ctrl+U clears whatever is typed into a dialog, as it clears the
    // composer: a search, a form field, or a key.
    if key.code == KeyCode::Char('u')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && app.overlay.is_filterable()
    {
        app.overlay_filter.clear();
        if !matches!(
            app.overlay,
            Overlay::Options {
                kind: crate::state::OptionsKind::ManagementField
                    | crate::state::OptionsKind::ConnectApiKey,
                ..
            }
        ) {
            app.selection = 0;
            app.preview_selected_theme();
        }
        return Ok(AppAction::Continue);
    }
    if app.workspace_mutation.is_some() || app.workspace_ref_operation.is_some() {
        app.notice =
            "Folder operation is still running · duplicate/cancel input is disabled".into();
        return Ok(AppAction::Continue);
    }
    if matches!(app.overlay, Overlay::Diff(_)) && handle_diff_key(app, key, client).await? {
        return Ok(AppAction::Continue);
    }
    if matches!(app.overlay, Overlay::SideChat) {
        return Ok(app.handle_side_key(key, client));
    }
    if matches!(app.overlay, Overlay::RemoteControl) {
        match key.code {
            KeyCode::Esc => app.close_overlay(),
            KeyCode::Enter => app.remote_control_primary(client).await?,
            KeyCode::Char('o') => app.open_remote_control_link(),
            KeyCode::Char('c') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.copy_remote_control_link();
            }
            KeyCode::Char('s') => app.stop_remote_control(client),
            _ => {}
        }
        return Ok(AppAction::Continue);
    }
    if matches!(app.overlay, Overlay::Chrome) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(AppAction::Continue);
        }
        match key.code {
            KeyCode::Esc => app.close_overlay(),
            KeyCode::Enter => app.chrome_primary(client),
            KeyCode::Char('e') => app.toggle_chrome(client),
            KeyCode::Char('d') => app.cycle_chrome_browser(client),
            KeyCode::Char('c') => app.copy_chrome_detail(),
            KeyCode::Char('o') => app.open_chrome_store(),
            // A short terminal scrolls the dialog.
            KeyCode::PageUp | KeyCode::Up => {
                let step = if key.code == KeyCode::PageUp { 10 } else { 1 };
                app.overlay_scroll = app.overlay_scroll.saturating_sub(step);
            }
            KeyCode::PageDown | KeyCode::Down => {
                let step = if key.code == KeyCode::PageDown { 10 } else { 1 };
                app.overlay_scroll = app
                    .overlay_scroll
                    .saturating_add(step)
                    .min(app.max_overlay_scroll.get());
            }
            KeyCode::Home => app.overlay_scroll = 0,
            KeyCode::End => app.overlay_scroll = app.max_overlay_scroll.get(),
            _ => {}
        }
        return Ok(AppAction::Continue);
    }
    if matches!(app.overlay, Overlay::ProviderLogin) {
        let failed = app
            .connect
            .login
            .as_ref()
            .is_some_and(|login| matches!(login.state, crate::connect::LoginState::Failed(_)));
        match key.code {
            // A failed sign-in has nothing left to cancel; Escape goes back to
            // the providers, as its "esc back" says.
            KeyCode::Esc if failed => {
                app.connect.login = None;
                app.open_connect_picker(None, client).await?;
            }
            KeyCode::Esc => app.cancel_login(client).await,
            KeyCode::Enter if failed => app.retry_login(client).await?,
            KeyCode::Enter | KeyCode::Char('o') => app.reopen_login_browser(),
            KeyCode::Char('c') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.copy_login_code();
            }
            _ => {}
        }
        return Ok(AppAction::Continue);
    }
    if matches!(
        app.overlay,
        Overlay::Panel { .. } | Overlay::Diff(_) | Overlay::Workflow { .. }
    ) {
        match key.code {
            KeyCode::Esc => app.close_overlay(),
            // The inspector is a report, not a picker: Enter hands the run to
            // its lifecycle controls instead of doing nothing.
            KeyCode::Enter => {
                if let Overlay::Workflow { ref target } = app.overlay {
                    match target.clone() {
                        crate::state::WorkflowTarget::Run(id) => {
                            app.open_cached_workflow_run_actions(id);
                        }
                        crate::state::WorkflowTarget::Invalid(_) => app.close_overlay(),
                        crate::state::WorkflowTarget::Definition(invocation) => {
                            app.close_overlay();
                            return app
                                .execute_command(&format!("/workflow run {invocation}"), client)
                                .await;
                        }
                    }
                }
            }
            KeyCode::PageUp | KeyCode::Up => {
                let step = if key.code == KeyCode::PageUp { 10 } else { 1 };
                app.overlay_scroll = app.overlay_scroll.saturating_sub(step);
            }
            KeyCode::PageDown | KeyCode::Down => {
                let step = if key.code == KeyCode::PageDown { 10 } else { 1 };
                app.overlay_scroll = app
                    .overlay_scroll
                    .saturating_add(step)
                    .min(app.max_overlay_scroll.get());
            }
            KeyCode::Home => app.overlay_scroll = 0,
            KeyCode::End => app.overlay_scroll = app.max_overlay_scroll.get(),
            _ => {}
        }
        return Ok(AppAction::Continue);
    }
    match key.code {
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::MessageActions,
                    ..
                }
            ) =>
        {
            app.open_messages(None);
        }
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::WorkspaceActions,
                    ..
                }
            ) =>
        {
            app.reopen_workspace_dashboard();
        }
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::WorkspaceResetConfirmation
                        | crate::state::OptionsKind::WorkspaceRemoveConfirmation,
                    ..
                }
            ) =>
        {
            app.reopen_workspace_actions();
        }
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::WorkspaceCreateBase,
                    ..
                }
            ) =>
        {
            let name = app.workspace_create_name.clone();
            app.open_workspace_create_name(name);
        }
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::WorkspaceCreateCustomBase,
                    ..
                }
            ) =>
        {
            app.workspace_custom_ref.clone_from(&app.overlay_filter);
            app.reopen_workspace_create_base_picker(Some("custom"));
        }
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::WorkspaceCreateName,
                    ..
                }
            ) =>
        {
            app.workspace_create_name = None;
            app.reopen_workspace_dashboard();
        }
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::GoalClearConfirmation,
                    ..
                }
            ) =>
        {
            app.open_goal_overlay();
        }
        KeyCode::Esc
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::ConnectMethod
                        | crate::state::OptionsKind::ConnectApiKey,
                    ..
                }
            ) =>
        {
            app.open_connect_picker(None, client).await?;
        }
        KeyCode::Esc => app.close_overlay(),
        KeyCode::Right | KeyCode::Tab if matches!(app.overlay, Overlay::Threads) => {
            app.open_selected_session_actions();
        }
        // Read before you launch: Enter on a definition starts it immediately,
        // so inspection needs its own key.
        KeyCode::Right | KeyCode::Tab
            if matches!(
                app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::WorkflowDashboard,
                    ..
                }
            ) =>
        {
            app.inspect_selected_workflow();
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.move_selection(-1);
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.move_selection(1);
        }
        KeyCode::Up => app.move_selection(-1),
        KeyCode::Down => app.move_selection(1),
        KeyCode::PageUp => {
            for _ in 0..10 {
                app.move_selection(-1);
            }
        }
        KeyCode::PageDown => {
            for _ in 0..10 {
                app.move_selection(1);
            }
        }
        KeyCode::Home => app.selection = 0,
        KeyCode::End => {
            app.selection = app.current_selection_length().saturating_sub(1);
        }
        KeyCode::Backspace if app.overlay.is_filterable() => {
            app.overlay_filter.pop();
            app.selection = 0;
            app.preview_selected_theme();
        }
        // The filter is a one-line prompt, so it erases like one: Ctrl+U
        // clears it and Ctrl+W takes back the last word.
        KeyCode::Char('u' | 'w')
            if app.overlay.is_filterable() && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if key.code == KeyCode::Char('u') {
                app.overlay_filter.clear();
            } else {
                let kept = app.overlay_filter.trim_end().len();
                let start = app.overlay_filter[..kept]
                    .char_indices()
                    .rfind(|(_, character)| character.is_whitespace())
                    .map_or(0, |(index, character)| index + character.len_utf8());
                app.overlay_filter.truncate(start);
            }
            app.selection = 0;
            app.preview_selected_theme();
        }
        KeyCode::Char(character)
            if app.overlay.is_filterable()
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            app.overlay_filter.push(character);
            app.selection = 0;
            app.preview_selected_theme();
        }
        KeyCode::Enter => {
            if let Some(choice) = app.selected_overlay_choice() {
                app.close_overlay();
                return app.execute_choice(choice, client).await;
            }
        }
        _ => {}
    }
    Ok(AppAction::Continue)
}
