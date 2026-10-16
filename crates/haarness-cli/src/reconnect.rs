//! Reconnect pacing shared by the Cloud connector and Chrome's bridge.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// How a client paces reconnects. The default is the Cloud connector's: each
/// attempt costs a runtime check, a token lookup and a Cloud database write,
/// so a relay that keeps taking the socket and dropping it must not get one a
/// second. A pacing whose `drops` is `u32::MAX` never reaches the long waits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pacing {
    /// The wait after a failed attempt, doubling per failure in a row up to `retry_max`.
    pub(crate) retry: Duration,
    pub(crate) retry_max: Duration,
    /// For this long after a proven connection ends, failed attempts wait at
    /// most `recover_max` instead: a host that was connected most likely lost
    /// its network for a moment (a stalled route, Wi-Fi after sleep), and each
    /// second of wait is a second the phone shows it offline. A longer outage
    /// (Cloud down, the computer off the network) backs off to `retry_max`
    /// again, and so does a client that never connected.
    pub(crate) recover_for: Duration,
    pub(crate) recover_max: Duration,
    /// A connection that stays up this long, or carries a request, proves the
    /// relay took this host. Only that resets the waits.
    pub(crate) stable: Duration,
    /// Connections in a row the relay took and dropped before they proved
    /// themselves, before the waits grow to minutes.
    pub(crate) drops: u32,
    /// The first of those long waits, doubling up to `cooldown_max`.
    pub(crate) cooldown: Duration,
    pub(crate) cooldown_max: Duration,
}
impl Default for Pacing {
    fn default() -> Self {
        Self {
            retry: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            recover_for: Duration::from_secs(5 * 60),
            recover_max: Duration::from_secs(10),
            stable: Duration::from_secs(60),
            drops: 4,
            cooldown: Duration::from_secs(60),
            cooldown_max: Duration::from_secs(10 * 60),
        }
    }
}
/// How one attempt to reach the relay ended, when reconnecting can still help.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// The handshake failed, or a connection that had not proven itself went
    /// silent: the network or Cloud is unreachable. It says nothing about the
    /// relay, so it is not a drop.
    Failed,
    /// Cloud refused the token.
    Unauthorized,
    /// The relay took the socket and the connection has ended. `proven` when
    /// it stayed up for the stable period or carried a request.
    Ended { proven: bool },
}
pub(crate) struct Backoff {
    pub(crate) pacing: Pacing,
    /// Attempts in a row that did not prove a connection.
    failures: u32,
    /// Of those, connections the relay took and then dropped.
    drops: u32,
    /// The longest wait logged so far, so each escalation is logged once.
    logged: Duration,
    /// When the last proven connection ended, for `Pacing::recover_for`.
    recovering: Option<Instant>,
}
impl Backoff {
    pub(crate) fn new(pacing: Pacing) -> Self {
        Self {
            pacing,
            failures: 0,
            drops: 0,
            logged: Duration::ZERO,
            recovering: None,
        }
    }
    /// The wait before the next attempt, after `link`.
    pub(crate) fn wait(&mut self, link: Link) -> Duration {
        self.wait_at(link, Instant::now())
    }
    fn wait_at(&mut self, link: Link, now: Instant) -> Duration {
        let pacing = self.pacing;
        match link {
            Link::Ended { proven: true } => {
                self.failures = 0;
                self.drops = 0;
                self.logged = Duration::ZERO;
                self.recovering = Some(now);
            }
            Link::Ended { proven: false } => self.drops = self.drops.saturating_add(1),
            // A failed connect says nothing about the relay; it keeps the count.
            Link::Failed | Link::Unauthorized => {}
        }
        self.failures = self.failures.saturating_add(1);
        if self.drops >= pacing.drops && matches!(link, Link::Ended { .. }) {
            let wait = pacing
                .cooldown
                .saturating_mul(1 << (self.drops - pacing.drops).min(16))
                .min(pacing.cooldown_max);
            if wait > self.logged {
                self.logged = wait;
                tracing::warn!(
                    drops = self.drops,
                    wait_secs = wait.as_secs(),
                    "remote relay keeps dropping the connection; backing off"
                );
                eprintln!(
                    "Haarness Cloud dropped {} connections in a row soon after accepting them; waiting {} min between attempts",
                    self.drops,
                    wait.as_secs().div_ceil(60).max(1)
                );
            }
            return jitter(wait);
        }
        let recovering = self
            .recovering
            .is_some_and(|ended| now.saturating_duration_since(ended) < pacing.recover_for);
        let most = if recovering {
            pacing.recover_max.min(pacing.retry_max)
        } else {
            pacing.retry_max
        };
        jitter(
            pacing
                .retry
                .saturating_mul(1 << (self.failures - 1).min(16))
                .min(most),
        )
    }
}
/// Up to a fifth shorter, so connectors that lost Cloud together do not return together.
pub(crate) fn jitter(wait: Duration) -> Duration {
    wait.mul_f64(1.0 - rand::random::<f64>() / 5.0)
}
/// How long a failure that keeps happening stays quiet before it is said again.
pub(crate) const REMIND: Duration = Duration::from_secs(10 * 60);
/// Says each way reconnecting fails once per outage: the first time a cause
/// is met, and again once [`REMIND`] has passed since it was last said, so a
/// long outage still shows. Repeats in between are only counted. An hour
/// without Cloud is then a few lines instead of one per attempt.
pub(crate) struct Outage {
    say: Box<dyn Fn(&str) + Send + Sync>,
    /// Each cause said this outage, and when it was last said.
    said: HashMap<String, Instant>,
    /// Attempts that failed since the last proven connection.
    attempts: u32,
    /// When the first of them failed.
    since: Option<Instant>,
}
impl Outage {
    pub(crate) fn new(say: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            say: Box::new(say),
            said: HashMap::new(),
            attempts: 0,
            since: None,
        }
    }
    /// Say a line that is not a failure (a connection made or ended).
    pub(crate) fn say(&self, line: &str) {
        (self.say)(line);
    }
    /// Whether an attempt failed since the last proven connection.
    pub(crate) fn ongoing(&self) -> bool {
        self.attempts > 0
    }
    /// Count a failed attempt, and say `line` (which names its cause) if it
    /// is news.
    pub(crate) fn failed(&mut self, line: &str) {
        self.failed_at(line, Instant::now());
    }
    fn failed_at(&mut self, line: &str, now: Instant) {
        self.attempts = self.attempts.saturating_add(1);
        self.since.get_or_insert(now);
        match self.said.get(line) {
            Some(last) if now.saturating_duration_since(*last) < REMIND => {}
            Some(_) => {
                self.said.insert(line.to_owned(), now);
                (self.say)(&format!(
                    "{line} (still failing after {} attempts)",
                    self.attempts
                ));
            }
            None => {
                self.said.insert(line.to_owned(), now);
                (self.say)(line);
            }
        }
    }
    /// A connection proved itself: the outage, if there was one, is over.
    /// Returns how many attempts it took, the one that worked included, and
    /// how long since the first failed.
    pub(crate) fn recovered(&mut self) -> Option<(u32, Duration)> {
        self.recovered_at(Instant::now())
    }
    fn recovered_at(&mut self, now: Instant) -> Option<(u32, Duration)> {
        self.said.clear();
        let attempts = std::mem::take(&mut self.attempts);
        let since = self.since.take()?;
        Some((
            attempts.saturating_add(1),
            now.saturating_duration_since(since),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// `wait` is `seconds`, jittered at most a fifth shorter.
    fn about(wait: Duration, seconds: u64) -> bool {
        let nominal = Duration::from_secs(seconds);
        wait <= nominal && wait >= nominal.mul_f64(0.8)
    }

    #[test]
    fn a_host_that_was_connected_retries_every_ten_seconds_for_five_minutes_then_every_minute() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        // Never connected: the waits double to a minute.
        let mut backoff = Backoff::new(Pacing::default());
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            let wait = backoff.wait_at(Link::Failed, start);
            assert!(about(wait, seconds), "{wait:?} for {seconds}s");
        }
        // A proven connection ended (the network stalled): at most 10 s apart
        // for the first five minutes of the outage.
        assert!(about(
            backoff.wait_at(Link::Ended { proven: true }, at(0)),
            1
        ));
        for (time, seconds) in [(1, 2), (3, 4), (7, 8), (15, 10), (25, 10), (290, 10)] {
            let wait = backoff.wait_at(Link::Failed, at(time));
            assert!(about(wait, seconds), "{wait:?} at {time}s for {seconds}s");
        }
        // A longer outage grows back to a minute.
        assert!(about(backoff.wait_at(Link::Failed, at(301)), 60));
        // The next proven connection starts a fast outage again.
        assert!(about(
            backoff.wait_at(Link::Ended { proven: true }, at(900)),
            1
        ));
        assert!(about(backoff.wait_at(Link::Failed, at(901)), 2));
        // Connections the relay keeps taking and dropping still wait minutes.
        let dropped = Link::Ended { proven: false };
        for seconds in [4, 8, 10, 60] {
            assert!(about(backoff.wait_at(dropped, at(910)), seconds));
        }
    }

    #[test]
    fn an_outage_says_each_cause_once_then_reminds_and_its_end_counts_the_attempts() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let heard = lines.clone();
        let mut outage = Outage::new(move |line| heard.lock().unwrap().push(line.to_owned()));
        let start = Instant::now();
        let refused = "Could not reach Haarness Cloud: connection refused; reconnecting";
        let timed_out = "Haarness Cloud did not answer in time; reconnecting";
        assert!(!outage.ongoing());
        assert_eq!(outage.recovered_at(start), None);
        // Two causes that take turns are each said once.
        for (offset, line) in [(0, refused), (1, timed_out), (2, refused), (3, timed_out)] {
            outage.failed_at(line, start + Duration::from_secs(offset));
        }
        for minute in 1..10 {
            outage.failed_at(refused, start + Duration::from_secs(60 * minute));
        }
        assert_eq!(*lines.lock().unwrap(), [refused, timed_out]);
        // Ten minutes after it was said, a cause that lasts is said again.
        outage.failed_at(refused, start + REMIND + Duration::from_secs(1));
        assert_eq!(
            lines.lock().unwrap().last().unwrap(),
            &format!("{refused} (still failing after 14 attempts)")
        );
        assert!(outage.ongoing());
        assert_eq!(
            outage.recovered_at(start + Duration::from_secs(900)),
            Some((15, Duration::from_secs(900)))
        );
        // The next outage says its causes afresh.
        assert!(!outage.ongoing());
        outage.failed_at(refused, start + Duration::from_secs(901));
        assert_eq!(lines.lock().unwrap().len(), 4);
    }
}
