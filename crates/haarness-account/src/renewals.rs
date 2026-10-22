//! Sign-in renewals in flight: Haarness Cloud's, ChatGPT's and xAI's.
//!
//! A renewal spends its refresh token the moment the server answers, and the
//! server ends the whole sign-in when a spent one comes back. So a renewal
//! runs on its own task ([`spawn`]): a caller that gives up (a Stop, a
//! timeout) returns at once while the renewal still saves what the server
//! sent. Each renewal is bounded ([`LIMIT`]) so a server that never answers
//! frees the lock the next request waits on, and a process that is about to
//! exit waits briefly for renewals still running ([`settle`]).

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// How long a renewal may wait for its server before it gives up.
pub const LIMIT: Duration = Duration::from_secs(30);

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static SETTLED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Counts a renewal until its task ends, however it ends.
struct InFlight;

impl InFlight {
    fn start() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        SETTLED.notify_waiters();
    }
}

/// Runs `renewal` on its own task, counted until it ends. Awaiting the
/// handle waits for it; dropping the handle leaves it running.
pub fn spawn<T: Send + 'static>(
    renewal: impl Future<Output = T> + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let counted = InFlight::start();
    tokio::spawn(async move {
        let _counted = counted;
        renewal.await
    })
}

/// Waits until no renewal is running, for at most `limit`: a process about to
/// exit calls this so a renewal Cloud already answered is saved, not lost
/// with the process.
pub async fn settle(limit: Duration) {
    let _ = tokio::time::timeout(limit, async {
        loop {
            let notified = SETTLED.notified();
            if IN_FLIGHT.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn settling_waits_for_a_running_renewal_but_not_past_its_limit() {
        let saved = Arc::new(AtomicBool::new(false));
        let flag = saved.clone();
        drop(spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            flag.store(true, Ordering::SeqCst);
        }));
        settle(Duration::from_secs(5)).await;
        assert!(
            saved.load(Ordering::SeqCst),
            "settle returned before the renewal saved"
        );

        drop(spawn(tokio::time::sleep(Duration::from_secs(60))));
        let started = std::time::Instant::now();
        settle(Duration::from_millis(100)).await;
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
