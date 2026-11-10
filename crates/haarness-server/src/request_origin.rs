//! Who sent a request: a person, or a program Haarness started on this computer (an agent's command, a hook, a
//! Haarness terminal). Only a person lets agents do more without asking, so the server asks this for the few requests
//! that would: raising a chat's approvals, a turn that approves its own requests, more reach for computer use.
//! Requests from another computer, and those the connector brings from phones and Haarness Cloud, come from people.
use haarness_core::AgentRuntime;
use haarness_protocol::{ApprovalMode, ApprovalModeSetting, StartTurnRequest, ThreadId};
use haarness_scheduler::Scheduler;
use std::{net::SocketAddr, sync::OnceLock};

/// Where schedules are kept, for telling a scheduled run's own turn (see [`use_schedules`]).
static SCHEDULES: OnceLock<Scheduler> = OnceLock::new();

/// Tells the server where schedules are kept. A scheduled run's turn approves its own requests, as the person who set
/// the schedule up allowed, and its runner is a program Haarness started too (by the server for "Run now", by
/// `haarness schedule run`): its turn, the schedule's own prompt, is told from an agent's by the schedule.
pub fn use_schedules(scheduler: Scheduler) {
    let _ = SCHEDULES.set(scheduler);
}

/// Whether the request reached this server from a local process Haarness started, read from the process table (a
/// `ps`, and an `lsof` to find the process): callers ask only for the requests that would give agents more reach.
pub(crate) async fn from_inside(peer: Option<SocketAddr>) -> bool {
    tokio::task::spawn_blocking(move || {
        peer.and_then(haarness_core::process_tree::local_client)
            .is_some_and(haarness_core::process_tree::request_from_inside)
    })
    .await
    .unwrap_or(false)
}

/// How much `mode` lets agents do without asking.
const fn reach(mode: ApprovalMode) -> u8 {
    match mode {
        ApprovalMode::Review => 0,
        ApprovalMode::Auto => 1,
        ApprovalMode::Yolo => 2,
    }
}

/// Whether `setting` would let the agents of chat `id` do more without asking than they may now. `Inherit` takes the
/// mode of the agent that delegated to the chat, or the server's default for a chat of its own.
pub(crate) async fn raises(
    runtime: &AgentRuntime,
    id: ThreadId,
    setting: ApprovalModeSetting,
) -> haarness_core::CoreResult<bool> {
    let wanted = match setting {
        ApprovalModeSetting::Review => return Ok(false),
        ApprovalModeSetting::Auto => ApprovalMode::Auto,
        ApprovalModeSetting::Yolo => ApprovalMode::Yolo,
        ApprovalModeSetting::Inherit => {
            let thread = runtime.get_thread(id).await?;
            match thread.parent_id.filter(|_| thread.is_subagent) {
                Some(parent) => match runtime.approval_mode(parent).await {
                    Ok(info) => info.mode,
                    Err(haarness_core::CoreError::NotFound(_)) => runtime.approval_mode_default(),
                    Err(error) => return Err(error),
                },
                None => runtime.approval_mode_default(),
            }
        }
    };
    Ok(reach(wanted) > reach(runtime.approval_mode(id).await?.mode))
}

/// Whether `request` is the turn of a run of the schedule chat `id` was made for: that schedule's prompt and agent,
/// with nothing added. Such a turn approves its own requests by the schedule's leave, whoever started the run.
pub(crate) async fn scheduled_run(
    runtime: &AgentRuntime,
    id: ThreadId,
    request: &StartTurnRequest,
) -> bool {
    let Some(scheduler) = SCHEDULES.get().cloned() else {
        return false;
    };
    let Ok(thread) = runtime.get_thread(id).await else {
        return false;
    };
    let Some(origin) = thread.schedule else {
        return false;
    };
    let plain = !request.synthetic
        && request.command.is_none()
        && request.attachments.is_empty()
        && request.resources.is_empty();
    let (text, agent) = (request.text.clone(), request.agent.clone());
    plain
        && tokio::task::spawn_blocking(move || {
            scheduler
                .find(&origin.id.to_string())
                .ok()
                .flatten()
                .is_some_and(|schedule| schedule.prompt == text && schedule.agent == agent)
        })
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    use haarness_core::{EventBus, StaticProviderRegistry};
    use haarness_protocol::{CreateThreadRequest, SetApprovalModeRequest};
    use std::sync::Arc;

    #[tokio::test]
    async fn only_more_reach_than_a_chat_has_now_counts_as_raising_it() {
        use ApprovalModeSetting::{Auto, Inherit, Review, Yolo};
        let runtime = AgentRuntime::new(
            Arc::new(haarness_store::SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(Vec::new())),
            Arc::new(crate::NativeToolRegistry::new(std::env::temp_dir())),
            EventBus::new(16),
        );
        let create = |parent| {
            runtime.create_thread(CreateThreadRequest {
                parent_id: parent,
                provider: Some("echo".into()),
                model: Some("echo-1".into()),
                ..Default::default()
            })
        };
        let set = |id, mode| {
            runtime.set_approval_mode(
                id,
                SetApprovalModeRequest {
                    mode,
                    revision: None,
                },
            )
        };
        let raised = async |id, settings: &[ApprovalModeSetting]| {
            let mut answers = Vec::new();
            for setting in settings {
                answers.push(raises(&runtime, id, *setting).await.unwrap());
            }
            answers
        };
        let all = [Review, Auto, Yolo, Inherit];
        // A chat of its own follows the server's default, Auto.
        let chat = create(None).await.unwrap().id;
        assert_eq!(raised(chat, &all).await, [false, false, true, false]);
        // In Review, Auto and Yolo raise it, and so does following the default again.
        set(chat, Review).await.unwrap();
        assert_eq!(raised(chat, &all).await, [false, true, true, true]);
        // In Yolo nothing does.
        set(chat, Yolo).await.unwrap();
        assert_eq!(raised(chat, &all).await, [false, false, false, false]);
        // An agent's chat inherits from the chat that started it.
        let agent = create(Some(chat)).await.unwrap().id;
        set(agent, Review).await.unwrap();
        assert_eq!(raised(agent, &all).await, [false, true, true, true]);
        set(chat, Auto).await.unwrap();
        assert_eq!(raised(agent, &all).await, [false, true, true, true]);
        set(agent, Auto).await.unwrap();
        assert_eq!(raised(agent, &all).await, [false, false, true, false]);
    }

    #[test]
    fn modes_rank_by_what_agents_may_do_unasked() {
        assert!(reach(ApprovalMode::Review) < reach(ApprovalMode::Auto));
        assert!(reach(ApprovalMode::Auto) < reach(ApprovalMode::Yolo));
    }
}
