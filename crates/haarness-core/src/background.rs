//! Background results a chat is still owed.
//!
//! Work a chat starts in the background (a child agent launched with
//! `background: true` or moved there, a workflow run started without waiting)
//! reports back by delivering its result into that chat, which starts a new
//! turn when the chat is idle. Until every such result is in, the chat is not
//! done, however its last turn ended. The runtime counts them in one place
//! (`AgentRuntime::background_results_owed`); this module holds what that
//! count reads besides the runtime's own child-task registry: results on
//! their way into the chat's input, and the probe the server installs to
//! read the workflow coordinator's pending deliveries.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use haarness_protocol::{RequestId, ThreadId};
use parking_lot::{Mutex, RwLock};

/// How many background workflow runs launched by a thread will still deliver
/// their terminal result into it. The server installs it once the workflow
/// coordinator exists: the coordinator already holds a weak reference to the
/// runtime, so the runtime cannot be built with it.
pub type BackgroundProbe = Arc<dyn Fn(ThreadId) -> u32 + Send + Sync>;

#[derive(Default)]
pub(crate) struct BackgroundLedger {
    probe: RwLock<Option<BackgroundProbe>>,
    /// Results handed to a thread's input and not yet admitted, by the
    /// request that carries each one.
    handoffs: Mutex<HashMap<ThreadId, Vec<RequestId>>>,
}

impl BackgroundLedger {
    pub(crate) fn set_probe(&self, probe: BackgroundProbe) {
        *self.probe.write() = Some(probe);
    }

    /// Background workflow runs that will deliver into `thread`.
    pub(crate) fn probed(&self, thread: ThreadId) -> u32 {
        let probe = self.probe.read().clone();
        probe.map_or(0, |probe| probe(thread))
    }

    /// Record a result on its way into `thread`, carried by `request`.
    /// `then` runs under the lock the count reads handoffs under, so a child
    /// task that hands its result over is never counted both as running and
    /// as on its way, nor as neither.
    pub(crate) fn hand_off(
        self: &Arc<Self>,
        thread: ThreadId,
        request: RequestId,
        then: impl FnOnce(),
    ) -> BackgroundHandoff {
        let mut handoffs = self.handoffs.lock();
        handoffs.entry(thread).or_default().push(request);
        then();
        drop(handoffs);
        BackgroundHandoff {
            ledger: Arc::clone(self),
            thread,
            request,
            released: AtomicBool::new(false),
        }
    }

    /// Run `count` with the handoffs to `thread` that are still owed, under
    /// the lock `hand_off` takes. `finishing` is the request of a turn that
    /// is finishing now: the result that turn carried in is not owed any
    /// more, even before the delivery that started it releases its handoff.
    pub(crate) fn with_handoffs<T>(
        &self,
        thread: ThreadId,
        finishing: Option<RequestId>,
        count: impl FnOnce(usize) -> T,
    ) -> T {
        let handoffs = self.handoffs.lock();
        let owed = handoffs.get(&thread).map_or(0, |requests| {
            requests
                .iter()
                .filter(|request| Some(**request) != finishing)
                .count()
        });
        count(owed)
    }
}

/// A background result on its way into a thread's input. It is owed until
/// the input is admitted (a steer the running turn will read, or the turn it
/// starts) or the delivery gives up; it is released then, at the latest when
/// this is dropped.
pub struct BackgroundHandoff {
    ledger: Arc<BackgroundLedger>,
    thread: ThreadId,
    request: RequestId,
    released: AtomicBool,
}

impl BackgroundHandoff {
    /// The request that carries the result into the thread.
    #[must_use]
    pub fn request_id(&self) -> RequestId {
        self.request
    }

    pub(crate) fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut handoffs = self.ledger.handoffs.lock();
        if let Some(requests) = handoffs.get_mut(&self.thread) {
            if let Some(index) = requests.iter().position(|request| *request == self.request) {
                requests.swap_remove(index);
            }
            if requests.is_empty() {
                handoffs.remove(&self.thread);
            }
        }
    }
}

impl Drop for BackgroundHandoff {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_is_owed_until_released_and_never_by_the_turn_it_started() {
        let ledger = Arc::new(BackgroundLedger::default());
        let thread = ThreadId::new();
        let owed = |finishing| ledger.with_handoffs(thread, finishing, |owed| owed);
        let first = ledger.hand_off(thread, RequestId::new(), || {});
        let second = ledger.hand_off(thread, RequestId::new(), || {});
        assert_eq!(owed(None), 2);
        assert_eq!(owed(Some(first.request_id())), 1);
        first.release();
        first.release();
        assert_eq!(owed(None), 1);
        drop(first);
        assert_eq!(owed(None), 1);
        drop(second);
        assert_eq!(owed(None), 0);
        assert!(ledger.handoffs.lock().is_empty());
    }

    #[test]
    fn the_probe_counts_workflow_deliveries_once_installed() {
        let ledger = BackgroundLedger::default();
        let thread = ThreadId::new();
        assert_eq!(ledger.probed(thread), 0);
        ledger.set_probe(Arc::new(move |asked| u32::from(asked == thread) * 2));
        assert_eq!(ledger.probed(thread), 2);
        assert_eq!(ledger.probed(ThreadId::new()), 0);
    }
}
