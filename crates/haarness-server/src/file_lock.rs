//! Per-file locks for the tools that change files (`write`, `edit`,
//! `multiedit`, `apply_patch`), so two calls on one file in this server
//! process (parallel child agents, two chats in one folder) take turns from
//! the first read to the end of the formatter pass instead of one losing the
//! other's change.
//!
//! A lock is keyed by the path the tool opens, after links. A call takes its
//! files' locks one at a time in sorted order, so two calls that share files
//! never wait on each other in a cycle. Waiting ends when the call is
//! stopped.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Weak};

use haarness_core::{CoreError, CoreResult};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

static LOCKS: LazyLock<parking_lot::Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(Default::default);

tokio::task_local! {
    /// Set while a call that already holds its files' locks runs the tool
    /// it wraps, which then takes none of them again.
    static HELD: ();
}

/// The locks a call holds; dropping it releases them.
#[derive(Default)]
pub(crate) struct FileLocks {
    held: Vec<(PathBuf, OwnedMutexGuard<()>)>,
}

/// Take the locks of `paths`, waiting for calls that hold any of them. A
/// call inside [`holding`] already holds its own and takes none.
pub(crate) async fn lock(
    paths: impl IntoIterator<Item = PathBuf>,
    cancel: &CancellationToken,
) -> CoreResult<FileLocks> {
    if HELD.try_with(|()| ()).is_ok() {
        return Ok(FileLocks::default());
    }
    let mut paths: Vec<PathBuf> = paths.into_iter().collect();
    paths.sort();
    paths.dedup();
    let mut locks = FileLocks::default();
    for path in paths {
        let lock = {
            let mut table = LOCKS.lock();
            match table.get(&path).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    table.insert(path.clone(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        let guard = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(CoreError::Cancelled),
            guard = lock.lock_owned() => guard,
        };
        locks.held.push((path, guard));
    }
    Ok(locks)
}

/// Run `future`, the tool a lock holder wraps, as that holder.
pub(crate) async fn holding<F: Future>(future: F) -> F::Output {
    HELD.scope((), future).await
}

impl Drop for FileLocks {
    fn drop(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.held.drain(..).map(|(path, _guard)| path).collect();
        // A lock nobody holds or waits for leaves the table.
        let mut table = LOCKS.lock();
        for path in paths {
            if table
                .get(&path)
                .is_some_and(|lock| lock.strong_count() == 0)
            {
                table.remove(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LOCKS, holding, lock};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn waits_for_the_holder_stops_on_cancel_and_leaves_no_entries() {
        let path = std::env::temp_dir().join(format!("haarness-lock-{}", uuid::Uuid::now_v7()));
        let cancel = CancellationToken::new();
        let first = lock([path.clone()], &cancel).await.unwrap();
        // The same file named twice is locked once.
        let waiting = tokio::time::timeout(
            Duration::from_millis(50),
            lock([path.clone(), path.clone()], &cancel),
        )
        .await;
        assert!(waiting.is_err(), "a second holder got the lock");
        // The tool a holder wraps takes nothing again.
        holding(async { lock([path.clone()], &cancel).await.unwrap() }).await;

        let stopped = CancellationToken::new();
        let stop = stopped.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            stop.cancel();
        });
        assert!(matches!(
            lock([path.clone()], &stopped).await,
            Err(haarness_core::CoreError::Cancelled)
        ));
        drop(first);
        let again = lock([path.clone(), PathBuf::from("/other")], &cancel)
            .await
            .unwrap();
        drop(again);
        assert!(!LOCKS.lock().contains_key(&path));
    }
}
