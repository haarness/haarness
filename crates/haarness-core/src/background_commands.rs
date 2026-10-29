//! Shell commands the agent runs, which may outlive their call.
//!
//! Every agent `bash` call is registered here under an id, so a client can
//! follow it and a foreground one can move to the background while it runs.
//! A command in the background keeps running after its call returned; when it
//! ends its chat is told. Ended commands stay listed for a while so a client
//! that looks a little later still finds how they ended.

use std::{collections::HashSet, sync::Arc, time::Duration};

use chrono::Utc;
use dashmap::DashMap;
use haarness_protocol::{ShellCommandInfo, ThreadId};
use parking_lot::Mutex;

/// How long an ended command stays listed.
pub(crate) const ENDED_KEPT: Duration = Duration::from_secs(10 * 60);

/// One command, as the routes and events report it.
pub(crate) struct CommandEntry {
    info: Mutex<ShellCommandInfo>,
}

impl CommandEntry {
    pub(crate) fn info(&self) -> ShellCommandInfo {
        self.info.lock().clone()
    }
}

/// The commands of every chat this runtime serves, by id.
#[derive(Default)]
pub(crate) struct ShellCommands {
    entries: DashMap<String, Arc<CommandEntry>>,
}

impl ShellCommands {
    /// The command `id`, while it is listed.
    pub(crate) fn get(&self, id: &str) -> Option<Arc<CommandEntry>> {
        self.forget_ended();
        self.entries.get(id).map(|entry| Arc::clone(entry.value()))
    }

    /// The commands of `threads` (every chat when `None`): live ones first,
    /// oldest first, then ended ones, the latest end first.
    pub(crate) fn list(&self, threads: Option<&HashSet<ThreadId>>) -> Vec<ShellCommandInfo> {
        self.forget_ended();
        let mut commands = self
            .entries
            .iter()
            .map(|entry| entry.value().info())
            .filter(|info| threads.is_none_or(|threads| threads.contains(&info.thread_id)))
            .collect::<Vec<_>>();
        commands.sort_by(|left, right| {
            right
                .state
                .is_live()
                .cmp(&left.state.is_live())
                .then_with(|| {
                    if left.state.is_live() {
                        left.started_at.cmp(&right.started_at)
                    } else {
                        right.ended_at.cmp(&left.ended_at)
                    }
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        commands
    }

    /// Commands of `threads` running in the background now.
    pub(crate) fn running_in_background(&self, threads: &HashSet<ThreadId>) -> u32 {
        let count = self
            .entries
            .iter()
            .filter(|entry| {
                let info = entry.value().info.lock();
                info.background && info.state.is_live() && threads.contains(&info.thread_id)
            })
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// Drop ended commands listed longer than [`ENDED_KEPT`].
    fn forget_ended(&self) {
        let Ok(kept) = chrono::Duration::from_std(ENDED_KEPT) else {
            return;
        };
        let cutoff = Utc::now() - kept;
        self.entries.retain(|_, entry| {
            let info = entry.info.lock();
            info.state.is_live() || info.ended_at.is_none_or(|ended| ended > cutoff)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haarness_protocol::{ShellCommandMovedBy, ShellCommandState, TurnId};

    fn command(id: &str, thread: ThreadId, state: ShellCommandState) -> ShellCommandInfo {
        ShellCommandInfo {
            id: id.into(),
            thread_id: thread,
            root_thread_id: thread,
            turn_id: TurnId::new(),
            call_id: format!("call-{id}"),
            command: "sleep 1".into(),
            workdir: None,
            background: true,
            moved_by: Some(ShellCommandMovedBy::Agent),
            state,
            exit_code: None,
            signal: None,
            ended_by: None,
            started_at: Utc::now(),
            background_at: Some(Utc::now()),
            ended_at: (!state.is_live()).then(Utc::now),
            stops_at: None,
            moves_at: None,
            output_bytes: 0,
            delivers_notice: true,
        }
    }

    fn insert(commands: &ShellCommands, info: ShellCommandInfo) {
        commands.entries.insert(
            info.id.clone(),
            Arc::new(CommandEntry {
                info: Mutex::new(info),
            }),
        );
    }

    #[test]
    fn live_commands_list_first_and_ended_ones_are_forgotten_after_a_while() {
        let commands = ShellCommands::default();
        let chat = ThreadId::new();
        let other = ThreadId::new();
        let mut old = command("old", chat, ShellCommandState::Exited);
        old.ended_at = Some(Utc::now() - chrono::Duration::minutes(11));
        insert(&commands, old);
        insert(
            &commands,
            command("ended", chat, ShellCommandState::Stopped),
        );
        insert(&commands, command("live", chat, ShellCommandState::Running));
        insert(
            &commands,
            command("elsewhere", other, ShellCommandState::Running),
        );
        let threads = HashSet::from([chat]);
        let listed = commands
            .list(Some(&threads))
            .into_iter()
            .map(|info| info.id)
            .collect::<Vec<_>>();
        assert_eq!(listed, ["live", "ended"]);
        assert!(commands.get("old").is_none());
        assert_eq!(commands.list(None).len(), 3);
        assert_eq!(commands.running_in_background(&threads), 1);
    }
}
