//! Reconnect pacing shared by the Cloud connector and Chrome's bridge.
use std::time::Duration;

/// How a client paces reconnects. The default is the Cloud connector's: each
/// attempt costs a runtime check, a token lookup and a Cloud database write,
/// so a relay that keeps taking the socket and dropping it must not get one a
/// second. A pacing whose `drops` is `u32::MAX` never reaches the long waits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pacing {
    /// The wait after a failed attempt, doubling per failure in a row up to `retry_max`.
    pub(crate) retry: Duration,
    pub(crate) retry_max: Duration,
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
            retry_max: Duration::from_secs(30),
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
    /// The handshake failed: the network or Cloud is unreachable.
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
}
impl Backoff {
    pub(crate) fn new(pacing: Pacing) -> Self {
        Self {
            pacing,
            failures: 0,
            drops: 0,
            logged: Duration::ZERO,
        }
    }
    /// The wait before the next attempt, after `link`.
    pub(crate) fn wait(&mut self, link: Link) -> Duration {
        let pacing = self.pacing;
        match link {
            Link::Ended { proven: true } => {
                self.failures = 0;
                self.drops = 0;
                self.logged = Duration::ZERO;
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
        jitter(
            pacing
                .retry
                .saturating_mul(1 << (self.failures - 1).min(16))
                .min(pacing.retry_max),
        )
    }
}
/// Up to a fifth shorter, so connectors that lost Cloud together do not return together.
pub(crate) fn jitter(wait: Duration) -> Duration {
    wait.mul_f64(1.0 - rand::random::<f64>() / 5.0)
}
