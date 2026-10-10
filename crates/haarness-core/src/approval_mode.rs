//! Per-chat approval mode. The server owns it so a TUI, Desktop window or
//! phone can never answer for a chat another client set to Review.
use crate::{CoreError, CoreResult, ThreadRepository};
use dashmap::DashMap;
use haarness_protocol::{ApprovalMode, ApprovalModeInfo, ApprovalModeSetting, ThreadId};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

const REVISION_KEY: &str = "approval_mode.revision";
/// Delegation chains are short; this only stops a corrupt parent cycle.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<ApprovalMode>,
    #[serde(default)]
    revision: u64,
}

pub(crate) struct ApprovalModes {
    repository: Arc<dyn ThreadRepository>,
    workspace: String,
    cache: DashMap<ThreadId, Stored>,
    revision: Mutex<Option<u64>>,
    /// Orders mode changes with permission admission: a request is either
    /// registered before Auto turns on, and released by the change, or it
    /// sees Auto and never prompts.
    pub(crate) gate: AsyncMutex<()>,
}

impl ApprovalModes {
    pub(crate) fn new(repository: Arc<dyn ThreadRepository>, workspace: String) -> Self {
        Self {
            repository,
            workspace,
            cache: DashMap::new(),
            revision: Mutex::new(None),
            gate: AsyncMutex::new(()),
        }
    }

    fn key(id: ThreadId) -> String {
        format!("approval_mode.thread.{id}")
    }

    async fn stored(&self, id: ThreadId) -> CoreResult<Stored> {
        if let Some(stored) = self.cache.get(&id) {
            return Ok(*stored);
        }
        let stored = self
            .repository
            .runtime_setting(&self.workspace, &Self::key(id))
            .await?
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        self.cache.insert(id, stored);
        Ok(stored)
    }

    /// The mode that applies to `id`: its own setting, else the setting of
    /// the agent that delegated to it, else Review. A Review anywhere up the
    /// chain wins, so switching a chat to Review stops its agents' Auto too.
    /// Forks keep their own.
    pub(crate) async fn effective(&self, id: ThreadId) -> CoreResult<ApprovalModeInfo> {
        let mut versions = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(id);
        let mut auto = None;
        while let Some(thread_id) = current {
            if !seen.insert(thread_id) || seen.len() > MAX_DEPTH {
                break;
            }
            let thread = match self.repository.get_thread(thread_id).await {
                Ok(thread) => thread,
                // A deleted ancestor ends the chain; an unknown chat is an error.
                Err(CoreError::NotFound(_)) if thread_id != id => break,
                Err(error) => return Err(error),
            };
            let stored = self.stored(thread_id).await?;
            versions.push(format!("{thread_id}:{}", stored.revision));
            match stored.mode {
                Some(ApprovalMode::Review) => {
                    return Ok(info(id, ApprovalMode::Review, thread_id, true, &versions));
                }
                Some(ApprovalMode::Auto) if auto.is_none() => auto = Some(thread_id),
                _ => {}
            }
            current = if thread.is_subagent { thread.parent_id } else { None };
        }
        Ok(match auto {
            Some(source) => info(id, ApprovalMode::Auto, source, true, &versions),
            None => info(id, ApprovalMode::Review, id, false, &versions),
        })
    }

    /// The delegating chats above `id`, nearest first.
    async fn ancestors(&self, id: ThreadId) -> CoreResult<Vec<ThreadId>> {
        let mut result = Vec::new();
        let mut current = self.repository.get_thread(id).await?;
        while current.is_subagent && result.len() < MAX_DEPTH {
            let Some(parent) = current.parent_id.filter(|parent| !result.contains(parent)) else {
                break;
            };
            result.push(parent);
            current = match self.repository.get_thread(parent).await {
                Ok(thread) => thread,
                Err(CoreError::NotFound(_)) => break,
                Err(error) => return Err(error),
            };
        }
        Ok(result)
    }

    pub(crate) async fn is_auto(&self, id: ThreadId) -> bool {
        match self.effective(id).await {
            Ok(info) => info.mode == ApprovalMode::Auto,
            Err(error) => {
                tracing::warn!(%error, thread = %id, "could not read the approval mode; asking instead");
                false
            }
        }
    }

    /// Store `setting` for `id`. The caller holds `gate`.
    pub(crate) async fn set(
        &self,
        id: ThreadId,
        setting: ApprovalModeSetting,
        expected: Option<&str>,
    ) -> CoreResult<ApprovalModeInfo> {
        let current = self.effective(id).await?;
        if expected.is_some_and(|expected| expected != current.revision) {
            return Err(CoreError::Conflict(
                "Approvals changed in another window. Refresh and try again.".into(),
            ));
        }
        if setting == ApprovalModeSetting::Auto {
            for ancestor in self.ancestors(id).await? {
                if self.stored(ancestor).await?.mode == Some(ApprovalMode::Review) {
                    return Err(CoreError::Conflict(
                        "The chat that started this agent is set to Review. Turn on Auto there instead."
                            .into(),
                    ));
                }
            }
        }
        let revision = self.next_revision().await?;
        let stored = Stored {
            mode: match setting {
                ApprovalModeSetting::Auto => Some(ApprovalMode::Auto),
                ApprovalModeSetting::Review => Some(ApprovalMode::Review),
                ApprovalModeSetting::Inherit => None,
            },
            revision,
        };
        let value = serde_json::to_value(stored).map_err(|error| CoreError::Storage(error.to_string()))?;
        self.repository
            .save_runtime_setting(&self.workspace, &Self::key(id), Some(value))
            .await?;
        self.cache.insert(id, stored);
        self.effective(id).await
    }

    /// One counter for the workspace, so a revision string changes even
    /// when a setting goes away and comes back.
    async fn next_revision(&self) -> CoreResult<u64> {
        let known = *self.revision.lock();
        let current = match known {
            Some(value) => value,
            None => self
                .repository
                .runtime_setting(&self.workspace, REVISION_KEY)
                .await?
                .and_then(|value| value.as_u64())
                .unwrap_or(0),
        };
        let next = current + 1;
        self.repository
            .save_runtime_setting(&self.workspace, REVISION_KEY, Some(next.into()))
            .await?;
        *self.revision.lock() = Some(next);
        Ok(next)
    }

    /// The top-level chat `id` belongs to, for event scoping.
    pub(crate) async fn root(&self, id: ThreadId) -> ThreadId {
        let mut root = id;
        let mut seen = HashSet::new();
        while seen.insert(root) && seen.len() <= MAX_DEPTH {
            match self.repository.get_thread(root).await {
                Ok(thread) if thread.is_subagent => match thread.parent_id {
                    Some(parent) => root = parent,
                    None => break,
                },
                _ => break,
            }
        }
        root
    }

    /// Chats whose effective mode may change with `id`: itself and the
    /// agents it delegated to, recursively (a Review reaches even those with
    /// their own Auto).
    pub(crate) async fn followers(&self, id: ThreadId) -> CoreResult<Vec<ThreadId>> {
        let threads = self.repository.list_threads(true).await?;
        let mut result = vec![id];
        let mut index = 0;
        while index < result.len() && result.len() < 10_000 {
            let parent = result[index];
            for thread in threads
                .iter()
                .filter(|thread| thread.is_subagent && thread.parent_id == Some(parent))
            {
                if !result.contains(&thread.id) {
                    result.push(thread.id);
                }
            }
            index += 1;
        }
        Ok(result)
    }
}

fn info(
    thread_id: ThreadId,
    mode: ApprovalMode,
    source_thread_id: ThreadId,
    explicit: bool,
    versions: &[String],
) -> ApprovalModeInfo {
    ApprovalModeInfo {
        thread_id,
        mode,
        source_thread_id,
        explicit,
        revision: versions.join("/"),
    }
}
