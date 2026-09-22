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
}
