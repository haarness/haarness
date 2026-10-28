//! File-system lookups a request waits for only so long. A path on a network
//! volume that went offline (a share whose server went to sleep, a mount on a
//! network the computer left), or a link into one, can hold a single call for
//! minutes. Such a lookup runs on a blocking thread, the request waits for it
//! until its deadline, and the thread is left to finish on its own.
//!
//! While a lookup of a path runs, asking about the same path again waits for
//! that lookup instead of starting another one. Once a lookup has outlived a
//! caller's deadline it counts as stuck: asking again answers at once that it
//! did not answer in time, so a path that hangs holds one blocking thread
//! however often it is asked about and costs each later request nothing.
//! When a stuck lookup does return, its answer is kept for [`KEPT`], so the
//! request after a path that is slow but works gets it at once.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

/// At most this many stuck lookups of one kind at a time. Past that, a new
/// one of that kind is not started and counts as not answered in time, so
/// paths that hang can never take every blocking thread the server has, and
/// stuck lookups of one kind (links, say) never keep another kind (folder
/// listings) from starting.
const MOST_STUCK: usize = 32;

/// How long the answer of a stuck lookup that returned is kept.
const KEPT: Duration = Duration::from_secs(10);

/// A lookup on its way: its answer, once it comes, and whether a caller's
/// deadline has passed while it ran.
struct Running {
    answer: Box<dyn Any + Send + Sync>,
    stuck: Arc<AtomicBool>,
}

type Key = (&'static str, PathBuf);

static RUNNING: LazyLock<parking_lot::Mutex<HashMap<Key, Running>>> =
    LazyLock::new(Default::default);

/// The answers of stuck lookups that returned, and when they did.
type Kept = HashMap<Key, (Instant, Box<dyn Any + Send + Sync>)>;

static KEPT_ANSWERS: LazyLock<parking_lot::Mutex<Kept>> = LazyLock::new(Default::default);

/// Takes a finished lookup off the table, also when it panicked.
struct Finished(Key);

impl Drop for Finished {
    fn drop(&mut self) {
        RUNNING.lock().remove(&self.0);
    }
}

/// The kept answer of a stuck `kind` lookup of `path` that returned within
/// the last [`KEPT`], without starting a lookup.
pub(crate) fn kept<T: Clone + 'static>(kind: &'static str, path: &Path) -> Option<T> {
    let mut kept = KEPT_ANSWERS.lock();
    kept.retain(|_, (returned, _)| returned.elapsed() < KEPT);
    kept.get(&(kind, path.to_path_buf()))
        .and_then(|(_, answer)| answer.downcast_ref::<T>())
        .cloned()
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
    if let Some(answer) = kept(kind, path) {
        return Some(answer);
    }
    if Instant::now() >= deadline {
        return None;
    }
    let key = (kind, path.to_path_buf());
    let (mut answer, stuck, start) = {
        let mut running = RUNNING.lock();
        if let Some(lookup) = running.get(&key) {
            if lookup.stuck.load(Ordering::Relaxed) {
                return None;
            }
            let answer = lookup
                .answer
                .downcast_ref::<watch::Receiver<Option<T>>>()?
                .clone();
            (answer, Arc::clone(&lookup.stuck), None)
        } else {
            let stuck = running
                .iter()
                .filter(|((other, _), lookup)| {
                    *other == kind && lookup.stuck.load(Ordering::Relaxed)
                })
                .count();
            if stuck >= MOST_STUCK {
                return None;
            }
            let (sender, answer) = watch::channel(None);
            let stuck = Arc::new(AtomicBool::new(false));
            running.insert(
                key.clone(),
                Running {
                    answer: Box::new(answer.clone()),
                    stuck: Arc::clone(&stuck),
                },
            );
            (answer, stuck, Some((sender, Finished(key))))
        }
    };
    // Started outside the table's lock: dropping a lookup that could not
    // start takes the table's lock itself.
    if let Some((sender, finished)) = start {
        let stuck = Arc::clone(&stuck);
        tokio::task::spawn_blocking(move || {
            let finished = finished;
            let found = look();
            if stuck.load(Ordering::Relaxed) {
                KEPT_ANSWERS.lock().insert(
                    finished.0.clone(),
                    (Instant::now(), Box::new(found.clone())),
                );
            }
            sender.send_replace(Some(found));
        });
    }
    match tokio::time::timeout_at(deadline, answer.wait_for(Option::is_some)).await {
        Ok(Ok(found)) => found.clone(),
        // The lookup panicked.
        Ok(Err(_)) => None,
        Err(_) => {
            stuck.store(true, Ordering::Relaxed);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MOST_STUCK, RUNNING, kept, within};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::Instant;

    fn unique(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("haarness-lookup-{name}-{}", uuid::Uuid::now_v7()))
    }

    /// A lookup that counts its starts and returns `true` once `released`.
    fn held(
        released: &Arc<AtomicBool>,
        started: &Arc<AtomicUsize>,
    ) -> impl FnOnce() -> bool + Send + 'static {
        let (released, started) = (Arc::clone(released), Arc::clone(started));
        move || {
            started.fetch_add(1, Ordering::SeqCst);
            while !released.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            true
        }
    }

    #[tokio::test]
    async fn a_stuck_path_holds_one_thread_and_its_late_answer_serves_the_next_ask() {
        let path = unique("stuck");
        let released = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));

        let begun = Instant::now();
        let soon = Instant::now() + Duration::from_millis(150);
        assert_eq!(
            within("test", &path, soon, held(&released, &started)).await,
            None
        );
        assert!(begun.elapsed() >= Duration::from_millis(150));
        assert!(begun.elapsed() < Duration::from_secs(5));

        // Asked again while it is stuck: no new lookup, and no waiting.
        let again = Instant::now();
        let later = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            within("test", &path, later, held(&released, &started)).await,
            None
        );
        assert!(again.elapsed() < Duration::from_millis(100));
        assert_eq!(started.load(Ordering::SeqCst), 1);

        // Once it returns, its answer serves the next ask at once.
        released.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while RUNNING.lock().contains_key(&("test", path.clone())) {
            assert!(
                std::time::Instant::now() < deadline,
                "the lookup never ended"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(kept::<bool>("test", &path), Some(true));
        let late = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            within("test", &path, late, held(&released, &started)).await,
            Some(true)
        );
        assert_eq!(started.load(Ordering::SeqCst), 1);
        // A lookup that answered in time is not kept.
        let quick = unique("quick");
        assert_eq!(within("test", &quick, late, || 7).await, Some(7));
        assert_eq!(kept::<i32>("test", &quick), None);
    }

    #[tokio::test]
    async fn asking_twice_about_a_path_starts_one_lookup_of_each_kind() {
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

    #[tokio::test]
    async fn stuck_lookups_of_one_kind_leave_room_for_the_others() {
        let released = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicUsize::new(0));
        let soon = Instant::now() + Duration::from_millis(100);
        let stuck = futures::future::join_all((0..MOST_STUCK).map(|index| {
            let path = unique(&format!("capped-{index}"));
            let look = held(&released, &started);
            async move { within("capped", &path, soon, look).await }
        }))
        .await;
        assert!(stuck.iter().all(Option::is_none));
        assert_eq!(started.load(Ordering::SeqCst), MOST_STUCK);

        // One more of that kind is not started; another kind still is.
        let later = Instant::now() + Duration::from_secs(5);
        let more = unique("capped-more");
        let refused = within("capped", &more, later, held(&released, &started)).await;
        assert_eq!(refused, None);
        assert_eq!(started.load(Ordering::SeqCst), MOST_STUCK);
        assert_eq!(
            within("other", &unique("other"), later, || 1).await,
            Some(1)
        );
        released.store(true, Ordering::SeqCst);
    }
}
