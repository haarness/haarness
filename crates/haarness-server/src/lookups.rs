//! File-system lookups a request waits for only so long. A path on a network
//! mount that cannot be reached right now (an automounted map, a share whose
//! server went to sleep), or a link into one, can hold a single call for
//! minutes. Such a lookup runs on a blocking thread, the request waits for it
//! until its deadline, and the thread is left to finish on its own.
//!
//! While a lookup of a path runs, asking about the same path again waits for
//! that lookup instead of starting another one, and once a lookup has outlived
//! a caller's deadline, asking again answers at once that it did not answer
//! in time, until it returns. So a path that hangs holds one blocking thread,
//! however often it is asked about, and costs each later request nothing.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use tokio::sync::watch;
use tokio::time::Instant;

/// At most this many lookups run at once. Past that (only paths that hang
/// pile up), a new one is not started and counts as not answered in time, so
/// they can never take every blocking thread the server has.
const MOST_RUNNING: usize = 64;

/// A lookup on its way: its answer, once it comes, and whether a caller's
/// deadline has passed while it ran.
struct Running {
    answer: Box<dyn Any + Send + Sync>,
    late: Arc<AtomicBool>,
}

type Key = (&'static str, PathBuf);

static RUNNING: LazyLock<parking_lot::Mutex<HashMap<Key, Running>>> =
    LazyLock::new(Default::default);

/// Takes a finished lookup off the table, also when it panicked.
struct Finished(Key);

impl Drop for Finished {
    fn drop(&mut self) {
        RUNNING.lock().remove(&self.0);
    }
}

/// Runs `look` on a blocking thread and waits for its answer until
/// `deadline`: `None` when it did not answer in time. `kind` names what is
/// looked up, so two kinds of lookup of one path never share an answer.
pub(crate) async fn within<T>(
    kind: &'static str,
    path: &Path,
    deadline: Instant,
    look: impl FnOnce() -> T + Send + 'static,
) -> Option<T>
where
    T: Clone + Send + Sync + 'static,
{
    if Instant::now() >= deadline {
        return None;
    }
    let key = (kind, path.to_path_buf());
    let (mut answer, late, start) = {
        let mut running = RUNNING.lock();
        if let Some(lookup) = running.get(&key) {
            if lookup.late.load(Ordering::Relaxed) {
                return None;
            }
            let answer = lookup
                .answer
                .downcast_ref::<watch::Receiver<Option<T>>>()?
                .clone();
            (answer, Arc::clone(&lookup.late), None)
        } else {
            if running.len() >= MOST_RUNNING {
                return None;
            }
            let (sender, answer) = watch::channel(None);
            let late = Arc::new(AtomicBool::new(false));
            running.insert(
                key.clone(),
                Running {
                    answer: Box::new(answer.clone()),
                    late: Arc::clone(&late),
                },
            );
            (answer, late, Some((sender, Finished(key))))
        }
    };
    // Started outside the table's lock: dropping a lookup that could not
    // start takes the table's lock itself.
    if let Some((sender, finished)) = start {
        tokio::task::spawn_blocking(move || {
            let _finished = finished;
            sender.send_replace(Some(look()));
        });
    }
    match tokio::time::timeout_at(deadline, answer.wait_for(Option::is_some)).await {
        Ok(Ok(found)) => found.clone(),
        // The lookup panicked.
        Ok(Err(_)) => None,
        Err(_) => {
            late.store(true, Ordering::Relaxed);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RUNNING, within};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::Instant;

    fn unique(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("haarness-lookup-{name}-{}", uuid::Uuid::now_v7()))
    }

    #[tokio::test]
    async fn a_lookup_that_hangs_is_waited_for_once_and_then_answered_late_at_once() {
        let path = unique("hangs");
        let released = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let hang = |released: Arc<AtomicBool>, started: Arc<AtomicUsize>| {
            move || {
                started.fetch_add(1, Ordering::SeqCst);
                while !released.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                true
            }
        };

        let begun = Instant::now();
        let first = within(
            "test",
            &path,
            Instant::now() + Duration::from_millis(150),
            hang(Arc::clone(&released), Arc::clone(&started)),
        )
        .await;
        assert_eq!(first, None);
        assert!(begun.elapsed() >= Duration::from_millis(150));
        assert!(begun.elapsed() < Duration::from_secs(5));

        // Asked again while it still hangs: no new lookup, and no waiting.
        let again = Instant::now();
        let second = within(
            "test",
            &path,
            Instant::now() + Duration::from_secs(5),
            hang(Arc::clone(&released), Arc::clone(&started)),
        )
        .await;
        assert_eq!(second, None);
        assert!(again.elapsed() < Duration::from_millis(100));
        assert_eq!(started.load(Ordering::SeqCst), 1);

        // Once it returns, the path is looked up afresh.
        released.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while RUNNING.lock().contains_key(&("test", path.clone())) {
            assert!(
                std::time::Instant::now() < deadline,
                "the lookup never ended"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let fresh = within(
            "test",
            &path,
            Instant::now() + Duration::from_secs(5),
            hang(Arc::clone(&released), Arc::clone(&started)),
        )
        .await;
        assert_eq!(fresh, Some(true));
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn callers_of_one_path_share_one_lookup_and_kinds_stay_apart() {
        let path = unique("shared");
        let started = Arc::new(AtomicUsize::new(0));
        let slow = |started: Arc<AtomicUsize>, answer: u32| {
            move || {
                started.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(100));
                answer
            }
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let (one, two, other) = tokio::join!(
            within("first", &path, deadline, slow(Arc::clone(&started), 1)),
            within("first", &path, deadline, slow(Arc::clone(&started), 2)),
            within("second", &path, deadline, slow(Arc::clone(&started), 3)),
        );
        assert_eq!((one, two, other), (Some(1), Some(1), Some(3)));
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }
}
