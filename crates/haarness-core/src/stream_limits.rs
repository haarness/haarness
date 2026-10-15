//! How long a model response may stay silent before Haarness stops waiting
//! for it. A provider, or a proxy between, can keep a connection open and
//! send nothing; without a limit the turn would wait until someone stops it,
//! and a schedule or a workflow child, which nobody watches, would never end.

use std::time::Duration;

use crate::CoreError;

/// The silence a streamed response may keep when its provider's
/// configuration says nothing: no bytes at all, counting the provider's
/// keep-alive pings and comments, from the request until the response ends.
pub const DEFAULT_STREAM_IDLE: Duration = Duration::from_secs(5 * 60);

/// How long a response that arrives in one piece (Amazon Bedrock's Converse)
/// may take in all when the configuration says nothing. Such a response sends
/// nothing until it is complete, so it needs room for a long answer.
pub const DEFAULT_RESPONSE_TOTAL: Duration = Duration::from_secs(15 * 60);

/// A provider's configured limits, carried on each request so the transport
/// can apply them where it waits on the network. `None` keeps the default;
/// a zero duration never gives up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamLimits {
    /// Longest silence of a streamed response (`stream_idle_timeout`).
    pub idle: Option<Duration>,
    /// Longest wait for a response that is not streamed (`request_timeout`).
    pub total: Option<Duration>,
}

impl StreamLimits {
    /// The silence after which a streamed response is given up, or `None` to
    /// wait for as long as it takes. A provider on this machine (`loopback`)
    /// waits by default: a local model sends nothing while it loads and reads
    /// a long prompt, which can take far longer than five minutes on a
    /// laptop, and a local server that dies closes its connection at once.
    #[must_use]
    pub fn idle(self, loopback: bool) -> Option<Duration> {
        match self.idle {
            Some(limit) => (!limit.is_zero()).then_some(limit),
            None => (!loopback).then_some(DEFAULT_STREAM_IDLE),
        }
    }

    /// The longest wait for a response that is not streamed, or `None`.
    #[must_use]
    pub fn total(self) -> Option<Duration> {
        match self.total {
            Some(limit) => (!limit.is_zero()).then_some(limit),
            None => Some(DEFAULT_RESPONSE_TOTAL),
        }
    }
}

const STALL: &str = "sent nothing for ";

/// The error for a response that stayed silent for `limit`. It names the
/// silence, never a reset connection: the provider closed nothing.
#[must_use]
pub fn stream_stall(limit: Duration) -> CoreError {
    CoreError::Provider(format!("the provider {STALL}{}", spoken(limit)))
}

/// The error for a response that is not streamed and was not complete within
/// `limit`. It is not retried: the answer may simply be long, and asking again
/// would pay for it again.
#[must_use]
pub fn response_timeout(limit: Duration) -> CoreError {
    CoreError::Provider(format!(
        "the provider sent no complete response within {}",
        spoken(limit)
    ))
}

/// How long a stalled response stayed silent ("5 minutes"), when `error` is
/// [`stream_stall`]'s.
#[must_use]
pub fn stream_stall_silence(error: &CoreError) -> Option<&str> {
    let (CoreError::Provider(message) | CoreError::ProviderIncomplete(message)) = error else {
        return None;
    };
    message.split_once(STALL).map(|(_, silence)| silence)
}

/// A duration the way the error says it: "5 minutes", "90 seconds".
fn spoken(limit: Duration) -> String {
    let unit =
        |count: u128, name: &str| format!("{count} {name}{}", if count == 1 { "" } else { "s" });
    let seconds = limit.as_secs();
    if limit.subsec_nanos() > 0 && seconds < 10 {
        unit(limit.as_millis(), "millisecond")
    } else if seconds >= 60 && seconds.is_multiple_of(60) {
        unit(u128::from(seconds / 60), "minute")
    } else {
        unit(u128::from(seconds), "second")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_providers_wait_five_minutes_and_local_ones_wait_until_configured() {
        let default = StreamLimits::default();
        assert_eq!(default.idle(false), Some(DEFAULT_STREAM_IDLE));
        assert_eq!(default.idle(true), None);
        let configured = StreamLimits {
            idle: Some(Duration::from_secs(90)),
            total: Some(Duration::ZERO),
        };
        assert_eq!(configured.idle(true), Some(Duration::from_secs(90)));
        assert_eq!(configured.total(), None);
        let off = StreamLimits {
            idle: Some(Duration::ZERO),
            total: None,
        };
        assert_eq!(off.idle(false), None);
        assert_eq!(off.total(), Some(DEFAULT_RESPONSE_TOTAL));
    }

    #[test]
    fn a_stall_names_the_silence() {
        let error = stream_stall(Duration::from_secs(300));
        assert_eq!(
            error.to_string(),
            "provider error: the provider sent nothing for 5 minutes"
        );
        assert_eq!(stream_stall_silence(&error), Some("5 minutes"));
        assert_eq!(
            stream_stall_silence(&stream_stall(Duration::from_secs(60))),
            Some("1 minute")
        );
        assert_eq!(
            stream_stall_silence(&stream_stall(Duration::from_secs(90))),
            Some("90 seconds")
        );
        assert_eq!(
            stream_stall_silence(&stream_stall(Duration::from_millis(250))),
            Some("250 milliseconds")
        );
        assert_eq!(
            stream_stall_silence(&CoreError::Provider("connection reset".into())),
            None
        );
        let timeout = response_timeout(Duration::from_secs(900));
        assert_eq!(
            timeout.to_string(),
            "provider error: the provider sent no complete response within 15 minutes"
        );
        assert_eq!(stream_stall_silence(&timeout), None);
    }
}
