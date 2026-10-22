use std::{collections::VecDeque, sync::Arc};

use chrono::Utc;
use haarness_protocol::{EventEnvelope, ItemId, SCHEMA_VERSION, ThreadId, TurnId};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

struct Inner {
    server_instance_id: Uuid,
    next_id: Mutex<u64>,
    replay: Mutex<VecDeque<EventEnvelope>>,
    capacity: usize,
    sender: broadcast::Sender<EventEnvelope>,
}

pub struct EventSubscription {
    pub replay: Vec<EventEnvelope>,
    pub receiver: broadcast::Receiver<EventEnvelope>,
    pub requires_resync: bool,
}

impl EventBus {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(16));
        Self {
            inner: Arc::new(Inner {
                server_instance_id: Uuid::now_v7(),
                next_id: Mutex::new(1),
                replay: Mutex::new(VecDeque::with_capacity(capacity)),
                capacity,
                sender,
            }),
        }
    }

    #[must_use]
    pub fn server_instance_id(&self) -> Uuid {
        self.inner.server_instance_id
    }

    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.inner.next_id.lock().saturating_sub(1)
    }

    /// Publish an event whose thread is its own root (or that has no thread).
    pub fn publish(
        &self,
        event_type: impl Into<String>,
        thread_id: Option<ThreadId>,
        turn_id: Option<TurnId>,
        item_id: Option<ItemId>,
        payload: Value,
    ) -> EventEnvelope {
        self.publish_scoped(event_type, thread_id, thread_id, turn_id, item_id, payload)
    }

    /// Publish with an explicit root thread — used for events raised on
    /// child-agent threads so clients following the root conversation see
    /// them.
    pub fn publish_scoped(
        &self,
        event_type: impl Into<String>,
        thread_id: Option<ThreadId>,
        root_thread_id: Option<ThreadId>,
        turn_id: Option<TurnId>,
        item_id: Option<ItemId>,
        payload: Value,
    ) -> EventEnvelope {
        // Serialize allocation through delivery. Concurrent agents must never
        // publish a newer cursor before an earlier approval or state update.
        let mut next = self.inner.next_id.lock();
        let event_id = *next;
        *next = next.saturating_add(1);
        let event = EventEnvelope {
            schema_version: SCHEMA_VERSION.into(),
            server_instance_id: self.inner.server_instance_id,
            event_id,
            event_type: event_type.into(),
            thread_id,
            root_thread_id,
            turn_id,
            item_id,
            payload,
            created_at: Utc::now(),
        };
        let mut replay = self.inner.replay.lock();
        replay.push_back(event.clone());
        while replay.len() > self.inner.capacity {
            replay.pop_front();
        }
        // Keep the replay lock through broadcast so subscribe() makes an atomic
        // handoff from replay to live delivery without losing or duplicating an event.
        let _ = self.inner.sender.send(event.clone());
        drop(replay);
        drop(next);
        event
    }

    #[must_use]
    pub fn subscribe(&self, after: Option<u64>) -> EventSubscription {
        let replay = self.inner.replay.lock();
        let oldest = replay.front().map(|event| event.event_id);
        let requires_resync = matches!((after, oldest), (Some(after), Some(oldest)) if after.saturating_add(1) < oldest);
        let events = if requires_resync {
            Vec::new()
        } else {
            replay
                .iter()
                .filter(|event| after.is_none_or(|id| event.event_id > id))
                .cloned()
                .collect()
        };
        EventSubscription {
            replay: events,
            receiver: self.inner.sender.subscribe(),
            requires_resync,
        }
    }
}

/// The most text one joined delta carries; a longer run goes out in several.
const JOINED_DELTA_LIMIT: usize = 64 * 1024;

/// Join `next` onto `run` when both are model text deltas of one kind
/// (`turn.text_delta` or `turn.reasoning_delta`) for the same turn of the same
/// chat, and say whether it did. The joined event carries `next`'s id and
/// time, so a client resuming from it skips both. A stream that fell behind
/// then sends one longer delta instead of each small one, and catches up
/// instead of losing its place; events of any other kind are never joined or
/// reordered, so a client that keeps up sees what it saw before.
pub fn join_delta(run: &mut EventEnvelope, next: &EventEnvelope) -> bool {
    if !matches!(
        run.event_type.as_str(),
        "turn.text_delta" | "turn.reasoning_delta"
    ) || run.event_type != next.event_type
        || run.server_instance_id != next.server_instance_id
        || run.thread_id != next.thread_id
        || run.root_thread_id != next.root_thread_id
        || run.turn_id != next.turn_id
        || run.item_id != next.item_id
    {
        return false;
    }
    let (Some(text), Some(more)) = (delta_text(&run.payload), delta_text(&next.payload)) else {
        return false;
    };
    if text.len() + more.len() > JOINED_DELTA_LIMIT {
        return false;
    }
    let joined = format!("{text}{more}");
    run.payload = serde_json::json!({ "delta": joined });
    run.event_id = next.event_id;
    run.created_at = next.created_at;
    true
}

/// A delta payload's text, when the payload is exactly `{"delta": text}`.
fn delta_text(payload: &Value) -> Option<&str> {
    let fields = payload.as_object()?;
    if fields.len() != 1 {
        return None;
    }
    fields.get("delta")?.as_str()
}

/// How much one connection's queue may hold, counted as text: a client
/// stalled for minutes behind a fast stream fits, one that stopped reading
/// does not hold more.
pub const PENDING_EVENTS_LIMIT: usize = 16 * 1024 * 1024;

/// The events waiting for one connection, oldest first. The connection takes
/// every event as it is published and keeps it here until its client reads
/// it, so a client slower than the model never falls out of the bus's bounded
/// queue; while deltas wait, each one continuing the run of the one before it
/// joins it (see [`join_delta`]), so the client reads fewer, longer ones and
/// catches up. Past its limit the queue gives up what it holds and the
/// client must reload what it shows.
pub struct PendingEvents {
    entries: VecDeque<(EventEnvelope, usize)>,
    size: usize,
    limit: usize,
}

impl PendingEvents {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            size: 0,
            limit,
        }
    }

    /// Queue `event`, or join it onto the last one waiting. Past the limit
    /// the queue empties and says how many events it let go.
    pub fn push(&mut self, event: EventEnvelope) -> Result<(), usize> {
        if let Some((last, size)) = self.entries.back_mut() {
            let added = delta_text(&event.payload).map_or(0, str::len);
            if join_delta(last, &event) {
                *size += added;
                self.size += added;
                return self.within_limit();
            }
        }
        let size = event_size(&event);
        self.size += size;
        self.entries.push_back((event, size));
        self.within_limit()
    }

    fn within_limit(&mut self) -> Result<(), usize> {
        if self.size <= self.limit {
            return Ok(());
        }
        let dropped = self.entries.len();
        self.entries.clear();
        self.size = 0;
        Err(dropped)
    }

    /// The oldest waiting event.
    pub fn pop(&mut self) -> Option<EventEnvelope> {
        let (event, size) = self.entries.pop_front()?;
        self.size -= size;
        Some(event)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// About how much text `event` takes on the wire: its payload, plus the
/// envelope's ids and names.
fn event_size(event: &EventEnvelope) -> usize {
    const ENVELOPE: usize = 320;
    ENVELOPE
        + delta_text(&event.payload).map_or_else(
            || serde_json::to_vec(&event.payload).map_or(0, |text| text.len()),
            str::len,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replays_events_after_the_requested_cursor() {
        let bus = EventBus::new(3);
        bus.publish("one", None, None, None, json!({"value": 1}));
        bus.publish("two", None, None, None, json!({"value": 2}));
        bus.publish("three", None, None, None, json!({"value": 3}));

        let subscription = bus.subscribe(Some(1));
        assert!(!subscription.requires_resync);
        assert_eq!(
            subscription
                .replay
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["two", "three"]
        );
    }

    #[test]
    fn requires_resync_when_cursor_falls_out_of_the_bounded_buffer() {
        let bus = EventBus::new(2);
        for value in 1..=4 {
            bus.publish("event", None, None, None, json!({"value": value}));
        }

        let subscription = bus.subscribe(Some(1));
        assert!(subscription.requires_resync);
        assert!(subscription.replay.is_empty());
    }

    #[test]
    fn concurrent_publishers_deliver_and_replay_in_cursor_order() {
        const WORKERS: usize = 8;
        const PER_WORKER: usize = 1000;
        let bus = EventBus::new(WORKERS * PER_WORKER);
        let mut subscription = bus.subscribe(Some(0));
        let start = std::sync::Barrier::new(WORKERS);
        std::thread::scope(|scope| {
            for worker in 0..WORKERS {
                let bus = &bus;
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    for index in 0..PER_WORKER {
                        bus.publish(
                            "permission.requested",
                            None,
                            None,
                            None,
                            json!({
                                "worker": worker, "index": index,
                            }),
                        );
                    }
                });
            }
        });
        let replay = bus.subscribe(Some(0)).replay;
        assert_eq!(replay.len(), WORKERS * PER_WORKER);
        for (index, event) in replay.iter().enumerate() {
            let expected = index as u64 + 1;
            assert_eq!(event.event_id, expected, "replay must follow cursor order");
            assert_eq!(
                subscription.receiver.try_recv().unwrap().event_id,
                expected,
                "live events must follow cursor order",
            );
        }
        assert_eq!(bus.cursor(), (WORKERS * PER_WORKER) as u64);
    }

    #[test]
    fn adjacent_deltas_of_one_turn_join_and_nothing_else_does() {
        let bus = EventBus::new(64);
        let (thread, other) = (ThreadId::new(), ThreadId::new());
        let (turn, later) = (TurnId::new(), TurnId::new());
        let delta = |kind: &str, thread: ThreadId, turn: TurnId, text: &str| {
            bus.publish(kind, Some(thread), Some(turn), None, json!({"delta": text}))
        };
        let events = vec![
            delta("turn.text_delta", thread, turn, "Hel"),
            delta("turn.text_delta", thread, turn, "lo"),
            delta("turn.reasoning_delta", thread, turn, "think"),
            delta("turn.reasoning_delta", thread, turn, "ing"),
            delta("turn.text_delta", thread, turn, " there"),
            bus.publish("item.created", Some(thread), Some(turn), None, json!({})),
            delta("turn.text_delta", thread, turn, "after"),
            delta("turn.text_delta", other, turn, "elsewhere"),
            delta("turn.text_delta", thread, later, "next turn"),
        ];
        let last_id = events[1].event_id;
        let mut pending = PendingEvents::new(PENDING_EVENTS_LIMIT);
        for event in events {
            pending.push(event).unwrap();
        }
        let joined: Vec<_> = std::iter::from_fn(|| pending.pop()).collect();
        let shown: Vec<_> = joined
            .iter()
            .map(|event| {
                (
                    event.event_type.as_str(),
                    event.payload["delta"].as_str().unwrap_or("-"),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("turn.text_delta", "Hello"),
                ("turn.reasoning_delta", "thinking"),
                ("turn.text_delta", " there"),
                ("item.created", "-"),
                ("turn.text_delta", "after"),
                ("turn.text_delta", "elsewhere"),
                ("turn.text_delta", "next turn"),
            ]
        );
        // A client resuming from the joined event skips both parts.
        assert_eq!(joined[0].event_id, last_id);
        assert!(
            joined
                .windows(2)
                .all(|pair| pair[0].event_id < pair[1].event_id)
        );
    }

    #[test]
    fn a_joined_delta_stays_under_its_limit_and_other_payloads_are_left_alone() {
        let bus = EventBus::new(8);
        let (thread, turn) = (ThreadId::new(), TurnId::new());
        let big = "x".repeat(JOINED_DELTA_LIMIT - 1);
        let mut run = bus.publish(
            "turn.text_delta",
            Some(thread),
            Some(turn),
            None,
            json!({"delta": big}),
        );
        let small = bus.publish(
            "turn.text_delta",
            Some(thread),
            Some(turn),
            None,
            json!({"delta": "yz"}),
        );
        assert!(!join_delta(&mut run, &small));
        assert_eq!(
            run.payload["delta"].as_str().unwrap().len(),
            JOINED_DELTA_LIMIT - 1
        );
        let mut extra = bus.publish(
            "turn.text_delta",
            Some(thread),
            Some(turn),
            None,
            json!({"delta": "a", "itemId": "x"}),
        );
        assert!(!join_delta(&mut extra, &small));
    }

    #[test]
    fn a_connection_that_reads_slowly_keeps_every_word_in_few_events() {
        let bus = EventBus::new(16);
        let (thread, turn) = (ThreadId::new(), TurnId::new());
        let mut pending = PendingEvents::new(PENDING_EVENTS_LIMIT);
        let mut written = String::new();
        // Far more than the bus keeps, while the client reads nothing.
        for index in 0..10_000 {
            let word = format!("w{index} ");
            written.push_str(&word);
            pending
                .push(bus.publish(
                    "turn.text_delta",
                    Some(thread),
                    Some(turn),
                    None,
                    json!({"delta": word}),
                ))
                .unwrap();
        }
        pending
            .push(bus.publish("item.created", Some(thread), Some(turn), None, json!({})))
            .unwrap();
        pending
            .push(bus.publish(
                "turn.text_delta",
                Some(thread),
                Some(turn),
                None,
                json!({"delta": "after"}),
            ))
            .unwrap();
        let mut text = String::new();
        let mut kinds = Vec::new();
        while let Some(event) = pending.pop() {
            if event.event_type == "turn.text_delta" && kinds.last() != Some(&"item.created") {
                text.push_str(event.payload["delta"].as_str().unwrap());
            }
            kinds.push(if event.event_type == "item.created" {
                "item.created"
            } else {
                "delta"
            });
        }
        assert_eq!(text, written);
        // Joined at 64 KiB, the run takes a few events; another kind ends it.
        assert!(kinds.len() < 10, "{}", kinds.len());
        assert_eq!(&kinds[kinds.len() - 2..], ["item.created", "delta"]);
        assert!(pending.is_empty());
    }

    #[test]
    fn a_connection_that_stops_reading_gives_up_past_its_limit() {
        let bus = EventBus::new(16);
        let thread = ThreadId::new();
        let mut pending = PendingEvents::new(64 * 1024);
        let big = "x".repeat(10_000);
        let mut overflowed = None;
        for index in 0..20 {
            let event = bus.publish(
                "item.created",
                Some(thread),
                None,
                None,
                json!({"text": big}),
            );
            if let Err(dropped) = pending.push(event) {
                overflowed = Some((index, dropped));
                break;
            }
        }
        let (index, dropped) = overflowed.expect("the queue gives up");
        assert!(index < 10 && dropped == index + 1, "{index} {dropped}");
        assert!(pending.is_empty());
        // It takes events again afterwards.
        pending
            .push(bus.publish("item.created", Some(thread), None, None, json!({})))
            .unwrap();
        assert_eq!(pending.len(), 1);
    }
}
