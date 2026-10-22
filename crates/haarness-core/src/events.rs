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
    // Appended where the run's text already is: joining a long run costs the
    // new text, not a copy of all of it.
    let Some(Value::String(text)) = run.payload.get_mut("delta") else {
        return false;
    };
    text.push_str(more);
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

/// How much memory one connection's queue may hold: a client stalled for
/// minutes behind a fast stream fits, one that stopped reading does not hold
/// more.
pub const PENDING_EVENTS_LIMIT: usize = 16 * 1024 * 1024;

/// What a queued entry costs beyond its text: the envelope's ids and names, a
/// delta's parsed payload, the queue's own bookkeeping.
const ENTRY_OVERHEAD: usize = 512;

/// One event as a connection sends it: its id (none for `resync_required`),
/// its name and its JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedEvent {
    pub id: Option<u64>,
    pub name: String,
    pub data: String,
}

impl EncodedEvent {
    fn of(event: &EventEnvelope) -> Self {
        Self {
            id: Some(event.event_id),
            name: event.event_type.clone(),
            data: serde_json::to_string(event).expect("event serializes"),
        }
    }
}

enum Entry {
    /// A delta, kept whole while later ones may still join it.
    Delta(EventEnvelope),
    /// Any other event, kept as the text it goes out as.
    Encoded(EncodedEvent),
}

/// The events waiting for one connection, oldest first. The connection takes
/// every event as it is published and keeps it here until its client reads
/// it, so a client slower than the model never falls out of the bus's bounded
/// queue; while deltas wait, each one continuing the run of the one before it
/// joins it (see [`join_delta`]), so the client reads fewer, longer ones and
/// catches up. Other events wait as the text they go out as, written once.
/// The limit counts what waits besides the largest entry, so one event
/// larger than the limit (a message with several screenshots) still goes
/// out. Past it, or after the bus dropped events for it, the queue lets go of
/// everything and holds only `resync_required`: the connection then ends, and
/// its client reconnects from its cursor.
pub struct PendingEvents {
    entries: VecDeque<(Entry, usize)>,
    size: usize,
    /// The largest waiting entry's size, when known.
    largest: Option<usize>,
    limit: usize,
    given_up: bool,
}

impl PendingEvents {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            size: 0,
            largest: Some(0),
            limit,
            given_up: false,
        }
    }

    /// Queue `event`, or join it onto the delta waiting last. Past the limit
    /// the queue gives up (see [`Self::give_up`]) and says how many events it
    /// let go. Once given up it takes nothing more.
    pub fn push(&mut self, event: EventEnvelope) -> Result<(), usize> {
        if self.given_up {
            return Err(0);
        }
        self.insert(event);
        if self.over_limit() {
            return Err(self.give_up());
        }
        Ok(())
    }

    fn insert(&mut self, event: EventEnvelope) {
        if let Some((Entry::Delta(last), size)) = self.entries.back_mut() {
            let added = delta_text(&event.payload).map_or(0, str::len);
            if join_delta(last, &event) {
                *size += added;
                self.size += added;
                let grown = *size;
                self.largest = self.largest.map(|largest| largest.max(grown));
                return;
            }
        }
        let delta = is_delta(&event)
            .then(|| delta_text(&event.payload).map(str::len))
            .flatten();
        let (entry, size) = match delta {
            Some(text) => (Entry::Delta(event), text),
            None => {
                let encoded = EncodedEvent::of(&event);
                let size = encoded.data.len() + encoded.name.len();
                (Entry::Encoded(encoded), size)
            }
        };
        let size = size + ENTRY_OVERHEAD;
        self.size += size;
        self.largest = self.largest.map(|largest| largest.max(size));
        self.entries.push_back((entry, size));
    }

    /// Whether what waits besides the largest entry is past the limit.
    fn over_limit(&mut self) -> bool {
        if self.size <= self.limit {
            return false;
        }
        let entries = &self.entries;
        let largest = *self
            .largest
            .get_or_insert_with(|| entries.iter().map(|(_, size)| *size).max().unwrap_or(0));
        self.size - largest > self.limit
    }

    /// Let go of every waiting event, and the memory they took, keeping only
    /// `resync_required`; returns how many events went.
    fn resync(&mut self) -> usize {
        let dropped = self.entries.len();
        let data = serde_json::json!({"type": "resync_required"});
        self.entries = VecDeque::from([(
            Entry::Encoded(EncodedEvent {
                id: None,
                name: "resync_required".into(),
                data: data.to_string(),
            }),
            0,
        )]);
        self.size = 0;
        self.largest = Some(0);
        dropped
    }

    /// Let go of every waiting event, as [`Self::resync`] does, and take
    /// nothing more: once `resync_required` is sent the connection ends.
    pub fn give_up(&mut self) -> usize {
        self.given_up = true;
        self.resync()
    }

    /// Whether the queue gave up: once what it holds is sent, the connection
    /// ends.
    #[must_use]
    pub fn given_up(&self) -> bool {
        self.given_up
    }

    /// The oldest waiting event, as it goes out.
    pub fn pop(&mut self) -> Option<EncodedEvent> {
        let (entry, size) = self.entries.pop_front()?;
        self.size -= size;
        if self.largest == Some(size) {
            self.largest = None;
        }
        Some(match entry {
            Entry::Delta(event) => EncodedEvent::of(&event),
            Entry::Encoded(encoded) => encoded,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How much memory the waiting events take, as the limit counts it.
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }
}

fn is_delta(event: &EventEnvelope) -> bool {
    matches!(
        event.event_type.as_str(),
        "turn.text_delta" | "turn.reasoning_delta"
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
        let joined = drained(&mut pending);
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

    /// What `pending` sends, read back as the events they were.
    fn drained(pending: &mut PendingEvents) -> Vec<EventEnvelope> {
        std::iter::from_fn(|| pending.pop())
            .map(|event| serde_json::from_str(&event.data).unwrap())
            .collect()
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
        // Counted as memory: the text, and a little per waiting event.
        assert!(
            pending.size() < written.len() + 8 * ENTRY_OVERHEAD,
            "{}",
            pending.size()
        );
        let events = drained(&mut pending);
        let kinds: Vec<_> = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        let text: String = events[..events.len() - 2]
            .iter()
            .map(|event| event.payload["delta"].as_str().unwrap())
            .collect();
        assert_eq!(text, written);
        // Joined at 64 KiB, the run takes a few events; another kind ends it.
        assert!(kinds.len() < 10, "{}", kinds.len());
        assert_eq!(
            &kinds[kinds.len() - 2..],
            ["item.created", "turn.text_delta"]
        );
        assert_eq!(pending.size(), 0);
    }

    #[test]
    fn a_connection_that_stops_reading_gives_up_past_its_limit_and_takes_nothing_more() {
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
        assert!(pending.given_up());
        // It holds only resync_required now, and takes nothing more: the
        // connection ends once that is sent.
        assert!(
            pending
                .push(bus.publish("item.created", Some(thread), None, None, json!({})))
                .is_err()
        );
        let marker = pending.pop().unwrap();
        assert_eq!((marker.id, marker.name.as_str()), (None, "resync_required"));
        assert!(pending.pop().is_none());
    }

    #[test]
    fn a_gap_in_what_the_bus_kept_leaves_only_resync_required() {
        let bus = EventBus::new(16);
        let thread = ThreadId::new();
        let mut pending = PendingEvents::new(PENDING_EVENTS_LIMIT);
        pending
            .push(bus.publish("item.created", Some(thread), None, None, json!({})))
            .unwrap();
        // The bus dropped events for this connection: nothing older than the
        // gap goes out after the marker, and nothing at all after it.
        assert_eq!(pending.give_up(), 1);
        let sent: Vec<_> = std::iter::from_fn(|| pending.pop())
            .map(|event| event.name)
            .collect();
        assert_eq!(sent, ["resync_required"]);
    }

    #[test]
    fn one_event_larger_than_the_limit_goes_out_and_the_limit_counts_what_waits_besides_it() {
        let bus = EventBus::new(16);
        let thread = ThreadId::new();
        let limit = 8 * 1024;
        let mut pending = PendingEvents::new(limit);
        let event = |text: &str| {
            bus.publish(
                "item.created",
                Some(thread),
                None,
                None,
                json!({"text": text}),
            )
        };
        let huge = "x".repeat(4 * limit);
        // A small event waits in front of one four times the limit: both
        // stay, and so do others behind them while they fit the limit.
        pending.push(event("first")).unwrap();
        pending.push(event(&huge)).unwrap();
        let mut behind = 0;
        while pending.push(event("behind")).is_ok() {
            behind += 1;
        }
        assert!(behind >= 4, "{behind}");
        assert!(pending.given_up());

        // Once the large one is out, another may take its place, but not two.
        let mut pending = PendingEvents::new(limit);
        pending.push(event(&huge)).unwrap();
        assert!(pending.pop().unwrap().data.contains(&huge));
        pending.push(event(&huge)).unwrap();
        assert_eq!(pending.push(event(&huge)), Err(2));

        // And once it is out, the limit counts all that waits again.
        let mut pending = PendingEvents::new(limit);
        pending.push(event(&huge)).unwrap();
        pending.pop().unwrap();
        let mut after = 0;
        while pending.push(event("behind")).is_ok() {
            after += 1;
        }
        assert!((behind..=behind + 2).contains(&after), "{behind} {after}");
    }

    #[test]
    fn a_run_grows_where_its_text_is_instead_of_being_copied_for_each_delta() {
        let bus = EventBus::new(16);
        let (thread, turn) = (ThreadId::new(), TurnId::new());
        let mut run = bus.publish("turn.text_delta", Some(thread), Some(turn), None, json!({}));
        let mut text = String::with_capacity(JOINED_DELTA_LIMIT);
        text.push('a');
        // Built by hand: `json!` would copy the text and its spare room away.
        let mut payload = serde_json::Map::new();
        payload.insert("delta".into(), Value::String(text));
        run.payload = Value::Object(payload);
        let start = run.payload["delta"].as_str().unwrap().as_ptr();
        for _ in 0..1000 {
            let next = bus.publish(
                "turn.text_delta",
                Some(thread),
                Some(turn),
                None,
                json!({"delta": "b"}),
            );
            assert!(join_delta(&mut run, &next));
        }
        let joined = run.payload["delta"].as_str().unwrap();
        assert_eq!(joined.len(), 1001);
        assert_eq!(joined.as_ptr(), start, "the run's text was copied");
    }
}
