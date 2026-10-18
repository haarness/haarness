//! When one provider request started answering and how long its answer took
//! to stream: the first-token time and output speed the Context panel and the
//! TUI's `/usage` show. Read off a monotonic clock, so a wall-clock step
//! between two reads cannot make either figure negative.

use std::time::{Duration, Instant};

/// One request's clock: when it was sent, and when its first and its newest
/// streamed content arrived (text, reasoning, a tool call or structured
/// output; usage and finish reports carry none). A retried request runs a
/// clock of its own.
#[derive(Debug, Clone, Copy)]
pub struct StreamClock {
    requested: Instant,
    first: Option<Instant>,
    last: Option<Instant>,
}

/// What a finished request records: milliseconds from sending it to its
/// first content, and from that first content to its last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamTiming {
    pub first_token_ms: u64,
    pub stream_ms: u64,
}

impl StreamClock {
    #[must_use]
    pub fn start(now: Instant) -> Self {
        Self {
            requested: now,
            first: None,
            last: None,
        }
    }

    /// Content arrived at `now`: the first one fixes the first-token time,
    /// every one moves the end of the stream.
    pub fn content(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// Nothing when no content streamed: there was no first token to time.
    #[must_use]
    pub fn timing(&self) -> Option<StreamTiming> {
        let first = self.first?;
        let last = self.last.unwrap_or(first);
        Some(StreamTiming {
            first_token_ms: millis(first.saturating_duration_since(self.requested)),
            stream_ms: millis(last.saturating_duration_since(first)),
        })
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_the_first_content_and_the_span_to_the_last() {
        let sent = Instant::now();
        let mut clock = StreamClock::start(sent);
        clock.content(sent + Duration::from_millis(1_250));
        clock.content(sent + Duration::from_millis(2_000));
        clock.content(sent + Duration::from_millis(3_750));
        assert_eq!(
            clock.timing(),
            Some(StreamTiming {
                first_token_ms: 1_250,
                stream_ms: 2_500,
            })
        );
    }

    #[test]
    fn a_request_that_streamed_nothing_records_nothing() {
        assert_eq!(StreamClock::start(Instant::now()).timing(), None);
    }

    #[test]
    fn one_piece_of_content_streams_for_no_time() {
        let sent = Instant::now();
        let mut clock = StreamClock::start(sent);
        clock.content(sent + Duration::from_millis(40));
        assert_eq!(
            clock.timing(),
            Some(StreamTiming {
                first_token_ms: 40,
                stream_ms: 0,
            })
        );
    }
}
