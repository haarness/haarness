//! Mouse handling: wheel scrolling plus row-click routing through the
//! frame's hit registry. A press released on the control it was made on, and
//! for the same request, is a click; anything else is left for
//! drag-selection.

use anyhow::Result;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::api::ApiClient;
use crate::input::AppAction;
use crate::state::{App, Overlay};
use crate::view::frame::HitTarget;

pub async fn handle_mouse(
    app: &mut App,
    event: MouseEvent,
    client: &ApiClient,
) -> Result<AppAction> {
    // A click on a chip answers the request on screen, never a staged or
    // selected choice left from another.
    app.follow_permission_head();
    match event.kind {
        MouseEventKind::ScrollUp => {
            app.hovered_cell = None;
            if app.overlay.is_selector() {
                for _ in 0..3 {
                    app.move_selection(-1);
                }
            } else if let Overlay::Diff(viewer) = &mut app.overlay {
                viewer.scroll_by(-3);
            } else if matches!(app.overlay, Overlay::SideChat) {
                app.wheel_side_chat(true);
            } else if matches!(app.overlay, Overlay::Panel { .. } | Overlay::Chrome) {
                app.overlay_scroll = app.overlay_scroll.saturating_sub(3);
            } else {
                app.scroll(3);
            }
        }
        MouseEventKind::ScrollDown => {
            app.hovered_cell = None;
            if app.overlay.is_selector() {
                for _ in 0..3 {
                    app.move_selection(1);
                }
            } else if let Overlay::Diff(viewer) = &mut app.overlay {
                viewer.scroll_by(3);
            } else if matches!(app.overlay, Overlay::SideChat) {
                app.wheel_side_chat(false);
            } else if matches!(app.overlay, Overlay::Panel { .. } | Overlay::Chrome) {
                app.overlay_scroll = app
                    .overlay_scroll
                    .saturating_add(3)
                    .min(app.max_overlay_scroll.get());
            } else {
                app.scroll(-3);
            }
        }
        MouseEventKind::Moved => move_hover(app, event.row as usize, event.column as usize),
        MouseEventKind::Down(MouseButton::Left) => {
            move_hover(app, event.row as usize, event.column as usize);
            app.mouse_down = Some(Press::at(app, event));
            app.selection_range = None;
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.hovered_cell = None;
            if let Some(mut press) = app.mouse_down.take() {
                press.moved = true;
                // Leaving the control, or the request it answers going, ends
                // the click for good: coming back is a selection.
                press.left |= !press.still_on(app, event);
                // A slip that stays on what was pressed is still a click
                // (see the release), so nothing is painted as selected.
                app.selection_range = (press.left || !press.slip(event))
                    .then_some((press.cell, (event.column, event.row)));
                app.mouse_down = Some(press);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let press = app.mouse_down.take();
            // A click is a release on the control the press was made on, for
            // the request that was on screen then: a card that appeared under
            // a held press, or a press beside a chip that ends on it, answers
            // nothing. A hand that slips a cell on a chip still presses it.
            let click_here = press.as_ref().is_some_and(|press| {
                press.still_on(app, event)
                    && if press.moved {
                        press.slip(event)
                    } else {
                        within_a_cell(press.cell, event)
                    }
            });
            if click_here {
                app.selection_range = None;
                return click(app, event.row as usize, event.column as usize, client).await;
            }
            if app.selection_range.is_some() {
                // A drag ended: copy the selection.
                let text = app.selected_text();
                if !text.trim().is_empty() {
                    crate::terminal::copy_to_clipboard(&text);
                    app.notice = "Copied selection".into();
                }
                app.selection_range = None;
            }
        }
        _ => {}
    }
    Ok(AppAction::Continue)
}

/// Whether the pointer is on the press's row, a cell from it at most.
fn within_a_cell((column, row): (u16, u16), event: MouseEvent) -> bool {
    event.row == row && event.column.abs_diff(column) <= 1
}

/// A left press waiting for its release: where it was made, the control it
/// was on in the frame on screen then and the request that control answers,
/// and whether the pointer has moved, or left that control, since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    cell: (u16, u16),
    target: Option<HitTarget>,
    request: Option<String>,
    moved: bool,
    left: bool,
}

impl Press {
    fn at(app: &App, event: MouseEvent) -> Self {
        let target = app.hit_at(event.row as usize, event.column as usize);
        Self {
            cell: (event.column, event.row),
            request: request_of(app, target.as_ref()),
            target,
            moved: false,
            left: false,
        }
    }

    /// Whether the pointer is on the control the press was made on, for the
    /// same request, and never left it.
    fn still_on(&self, app: &App, event: MouseEvent) -> bool {
        let target = app.hit_at(event.row as usize, event.column as usize);
        !self.left && target == self.target && request_of(app, target.as_ref()) == self.request
    }

    /// Whether a press that moved is a slip of the hand on a control: a cell
    /// at most, on something a click acts on. Text is there to be selected:
    /// a dialog's own surface, a review row's code, the conversation.
    fn slip(&self, event: MouseEvent) -> bool {
        within_a_cell(self.cell, event)
            && self.target.as_ref().is_some_and(|target| {
                !matches!(
                    target,
                    HitTarget::DialogSurface
                        | HitTarget::DialogItem(_)
                        | HitTarget::DiffBodyRow(_)
                        | HitTarget::DiffTreeRow(_)
                )
            })
    }
}

/// The request a control answers, so a card or list replaced under a held
/// press is not acted on by its release: a permission's id (and whether an
/// "always" answer waits for its confirmation, which changes what the chips
/// do), a question's id, a list row's list and item.
fn request_of(app: &App, target: Option<&HitTarget>) -> Option<String> {
    match target? {
        HitTarget::PermissionOption(_) | HitTarget::PermissionFocus => app
            .first_pending_permission()
            .map(|pending| format!("{}:{:?}", pending.permission_id, app.confirm_always)),
        HitTarget::QuestionChoice(_) => app
            .first_pending_question()
            .map(|pending| pending.question_id.clone()),
        HitTarget::MenuRow(index) => Some(app.menu_row_key(*index)),
        _ => None,
    }
}

async fn click(app: &mut App, row: usize, column: usize, client: &ApiClient) -> Result<AppAction> {
    let Some(target) = app.hit_at(row, column) else {
        if app.workspace_mutation.is_some() || app.workspace_ref_operation.is_some() {
            app.notice = "Folder operation is still running · it cannot be safely cancelled".into();
        } else if !matches!(app.overlay, Overlay::None) {
            app.close_overlay();
        }
        return Ok(AppAction::Continue);
    };
    match target {
        HitTarget::ToolToggle(item) => {
            if !app.expanded.remove(&item) {
                app.expanded.insert(item);
            }
        }
        HitTarget::TaskOpen(call_id) => {
            if let Some(child) = app.children.get(&call_id).map(|task| task.child_thread_id) {
                app.enter_child(child, client).await?;
            }
        }
        HitTarget::WorkflowOpen(run_id) => {
            app.open_workflow_run(run_id, client).await?;
        }
        HitTarget::ChildParent => {
            app.leave_child(client).await?;
        }
        HitTarget::ChildPrevious => {
            app.cycle_child(-1, client).await?;
        }
        HitTarget::ChildNext => {
            app.cycle_child(1, client).await?;
        }
        HitTarget::ThreadOpen(thread_id) => {
            if let Some(thread) = app
                .threads
                .iter()
                .find(|candidate| candidate.id == thread_id)
                .cloned()
            {
                app.switch_thread(thread, client).await?;
            }
        }
        HitTarget::DiffTreeRow(index) => {
            if let Overlay::Diff(viewer) = &mut app.overlay {
                viewer.focus = crate::diffview::DiffFocus::Tree;
                viewer.tree_selection = index;
                viewer.activate_tree_row();
            }
        }
        HitTarget::DiffBodyRow(index) => {
            if let Overlay::Diff(viewer) = &mut app.overlay
                && viewer
                    .display
                    .get(index)
                    .is_some_and(crate::diffview::DisplayRow::selectable)
            {
                viewer.focus = crate::diffview::DiffFocus::Patch;
                viewer.cursor = Some(index);
                if let Some(file) = viewer.display[index].file() {
                    viewer.active_file = file;
                }
            }
        }
        HitTarget::MenuRow(index) => return activate_menu_row(app, index, client).await,
        HitTarget::PermissionFocus => {
            if !app.pending_permissions.is_empty() {
                app.permission_focus = true;
                app.permission_typed = None;
                app.update_permission_notice();
            }
        }
        HitTarget::PermissionOption(index) => {
            if app.first_pending_permission().is_some() {
                app.permission_focus = true;
                app.permission_typed = None;
                if let Some(staged) = app.confirm_always {
                    // Confirm/cancel chips while an "always" answer is staged.
                    if index == 0 {
                        crate::input::answer_permission(app, client, staged).await?;
                    } else {
                        app.confirm_always = None;
                        app.permission_selection = 0;
                        app.update_permission_notice();
                    }
                } else if let Some(choice) = crate::state::PermissionChoice::available(app)
                    .get(index)
                    .copied()
                {
                    app.permission_selection = index;
                    crate::input::answer_permission(app, client, choice.key()).await?;
                }
                app.hovered_cell = None;
            }
        }
        HitTarget::SidebarSection(index) => {
            if !app.sidebar_collapsed.remove(&index) {
                app.sidebar_collapsed.insert(index);
            }
        }
        HitTarget::SidebarToggle => {
            app.toggle_sidebar();
            if app.sidebar_open {
                app.mcp_status = client.mcp().await.unwrap_or_default();
            }
        }
        HitTarget::QuestionChoice(index) => {
            if app.is_child_view() {
                app.notice = "Return to the parent to answer".into();
                return Ok(AppAction::Continue);
            }
            let Some((id, multiple)) = app
                .first_pending_question()
                .map(|pending| (pending.question_id.clone(), pending.multiple))
            else {
                return Ok(AppAction::Continue);
            };
            app.question_selection = index;
            if multiple {
                app.toggle_question_choice(index);
                app.hovered_cell = None;
                return Ok(AppAction::Continue);
            }
            let Some(answer) = app.question_choice(index) else {
                return Ok(AppAction::Continue);
            };
            client
                .reply_question(&id, haarness_protocol::QuestionReply { answer })
                .await?;
            app.question_answered(&id);
            app.hovered_cell = None;
        }
        HitTarget::QueueList => app.open_input_queue(client).await?,
        HitTarget::QueueItem(id) => app.open_queue_item_actions(id, false)?,
        HitTarget::QueueEditCancel => {
            if app.cancel_queue_edit() {
                app.notice = "Edit cancelled · draft restored".into();
            }
        }
        HitTarget::DialogSurface | HitTarget::DialogItem(_) => {}
        HitTarget::ToastDismiss => {
            app.notification_toast = None;
        }
    }
    Ok(AppAction::Continue)
}

fn move_hover(app: &mut App, row: usize, column: usize) {
    app.hovered_cell = Some((
        u16::try_from(column).unwrap_or(u16::MAX),
        u16::try_from(row).unwrap_or(u16::MAX),
    ));
    match app.hit_at(row, column) {
        Some(HitTarget::MenuRow(index)) => app.selection = index,
        Some(HitTarget::QuestionChoice(index)) => app.question_selection = index,
        Some(HitTarget::PermissionOption(index)) => app.permission_selection = index,
        _ => {}
    }
}

/// Clicking a menu row selects and activates it — the same effect as moving
/// the selection there and pressing Enter.
async fn activate_menu_row(app: &mut App, index: usize, client: &ApiClient) -> Result<AppAction> {
    app.selection = index;
    if !matches!(app.overlay, Overlay::None) {
        if let Some(choice) = app.selected_overlay_choice() {
            app.close_overlay();
            return app.execute_choice(choice, client).await;
        }
        return Ok(AppAction::Continue);
    }
    if let Some(commands) = app.slash_suggestions() {
        if let Some(entry) = crate::state::selected(&commands, index) {
            app.composer.set_text(format!("/{} ", entry.name));
        }
        return Ok(AppAction::Continue);
    }
    if crate::input::apply_mention(app, client) {
        crate::input::refresh_mentions(app, client);
    }
    Ok(AppAction::Continue)
}
