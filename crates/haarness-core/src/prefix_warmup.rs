//! Prefix warm-up for concurrently started child agents.
//!
//! A workflow fan-out or a batch of task delegations starts several child
//! turns at once, and their first requests share one prompt prefix (tools and
//! system prompt). A provider that writes its prompt cache explicitly makes an
//! entry readable only once the response to the request that wrote it begins,
//! so every sibling sent before then writes the same prefix again instead of
//! reading it. The first request for a prefix is the leader and goes at once;
//! the others (followers) wait until its response begins, then go and read
//! what it wrote. Only the timing of a follower's request moves: the request
//! itself is never changed.
//!
//! Fails open everywhere. A follower waits at most `wait` from the leader's
//! start, so a leader that hangs holds nobody past it and the next arrival
//! replaces it; a leader that fails, ends without a response or is dropped
//! (cancelled) releases its followers at once and leaves no warm mark; a
//! follower's own cancellation ends its wait.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::Stream;
use parking_lot::Mutex;
use tokio::{sync::watch, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{CoreError, CoreResult, ProviderEvent, ProviderRequest, ProviderStream};

/// Longest a follower waits, counted from the leader's start.
const WAIT: Duration = Duration::from_secs(20);
/// How long a leader whose response began keeps its prefix warm, counted from
/// its send: the shorter Anthropic cache tier, so never overstated.
const WARM: Duration = Duration::from_secs(5 * 60);
/// Most prefixes remembered at once.
const CAPACITY: usize = 256;

/// The shared part of a request, hashed: provider, cache scope (endpoint and
/// credentials), model, system prompt, tools and request options. The
/// messages and the per-thread cache key are left out, since they are what
/// differs between siblings. No prompt text or credential is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PrefixKey(u64);

impl PrefixKey {
    pub(crate) fn new(provider: &str, scope: &str, request: &ProviderRequest) -> Self {
        let mut hasher = DefaultHasher::new();
        provider.hash(&mut hasher);
        scope.hash(&mut hasher);
        request.model.hash(&mut hasher);
        request.system.hash(&mut hasher);
        request.tools.len().hash(&mut hasher);
        for tool in &request.tools {
            tool.name.hash(&mut hasher);
            tool.description.hash(&mut hasher);
            tool.input_schema.to_string().hash(&mut hasher);
        }
        request.reasoning_effort.hash(&mut hasher);
        request
            .output_schema
            .as_ref()
            .map(ToString::to_string)
            .hash(&mut hasher);
        let parameters = &request.parameters;
        parameters.temperature.map(f64::to_bits).hash(&mut hasher);
        parameters.top_p.map(f64::to_bits).hash(&mut hasher);
        parameters.top_k.hash(&mut hasher);
        parameters.max_output_tokens.hash(&mut hasher);
        parameters.headers.hash(&mut hasher);
        parameters.options.len().hash(&mut hasher);
        for (name, value) in &parameters.options {
            name.hash(&mut hasher);
            value.to_string().hash(&mut hasher);
        }
        Self(hasher.finish())
    }
}

impl std::fmt::Display for PrefixKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

struct Slot {
    /// Tells this leader from one that replaced it; also the eviction order.
    id: u64,
    /// The leader's start.
    since: Instant,
    /// Held until the leader's response begins; dropping it releases the
    /// followers. `None` once the prefix is warm.
    open: Option<watch::Sender<()>>,
}

#[derive(Default)]
struct Slots {
    map: HashMap<PrefixKey, Slot>,
    next: u64,
}

/// Leaders, warm marks and their limits for one runtime. Clones share them.
#[derive(Clone)]
pub(crate) struct PrefixWarmup {
    slots: Arc<Mutex<Slots>>,
    wait: Duration,
    warm: Duration,
    capacity: usize,
}

impl Default for PrefixWarmup {
    fn default() -> Self {
        Self::with_limits(WAIT, WARM, CAPACITY)
    }
}

enum Seen {
    Follow(watch::Receiver<()>, Instant),
    Warm,
    Lead,
}

impl PrefixWarmup {
    fn with_limits(wait: Duration, warm: Duration, capacity: usize) -> Self {
        Self {
            slots: Arc::default(),
            wait,
            warm,
            capacity,
        }
    }

    /// Returns the lead when the caller is this prefix's leader, or `None` once
    /// the caller may go: a follower returns after the leader's response began
    /// or its wait ended, and a request inside the warm window never waits.
    /// The check and the claim happen under one lock, so two callers cannot
    /// both lead.
    pub(crate) async fn enter(
        &self,
        key: PrefixKey,
        cancel: &CancellationToken,
    ) -> CoreResult<Option<Lead>> {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let now = Instant::now();
        let (mut open, deadline) = {
            let mut slots = self.slots.lock();
            let seen = match slots.map.get(&key) {
                Some(slot) => match &slot.open {
                    Some(open) if now < slot.since + self.wait => {
                        Seen::Follow(open.subscribe(), slot.since + self.wait)
                    }
                    None if now < slot.since + self.warm => Seen::Warm,
                    // An expired leader or warm mark: lead afresh.
                    _ => Seen::Lead,
                },
                None => Seen::Lead,
            };
            match seen {
                Seen::Follow(open, deadline) => (open, deadline),
                Seen::Warm => {
                    tracing::debug!(%key, "prompt prefix warm: request goes at once");
                    return Ok(None);
                }
                Seen::Lead => {
                    tracing::debug!(%key, "prompt prefix leader: request goes first");
                    return Ok(Some(self.lead(&mut slots, key, now)));
                }
            }
        };
        tracing::debug!(
            %key,
            wait_ms = u64::try_from((deadline - now).as_millis()).unwrap_or(u64::MAX),
            "prompt prefix follower: waiting for the leader's response"
        );
        let why = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(CoreError::Cancelled),
            // Never sent to: this ends when the leader drops its sender.
            _ = open.changed() => "released",
            () = tokio::time::sleep_until(deadline) => "timeout",
        };
        tracing::debug!(%key, why, "prompt prefix follower: request goes");
        Ok(None)
    }

    fn lead(&self, slots: &mut Slots, key: PrefixKey, now: Instant) -> Lead {
        slots.next += 1;
        let id = slots.next;
        let (open, _) = watch::channel(());
        // Replacing an expired leader drops its sender, releasing anyone still
        // waiting on it; its own end later finds another id and does nothing.
        slots.map.insert(
            key,
            Slot {
                id,
                since: now,
                open: Some(open),
            },
        );
        if slots.map.len() > self.capacity {
            let warm = self.warm;
            slots
                .map
                .retain(|_, slot| slot.open.is_some() || now < slot.since + warm);
        }
        while slots.map.len() > self.capacity {
            // Evicting a pending leader drops its sender: its followers go.
            let Some(oldest) = slots
                .map
                .iter()
                .min_by_key(|(_, slot)| slot.id)
                .map(|(key, _)| *key)
            else {
                break;
            };
            slots.map.remove(&oldest);
        }
        Lead {
            slots: Arc::clone(&self.slots),
            key,
            id,
            ended: false,
        }
    }
}

/// The leader's claim on a prefix. Its response beginning marks the prefix
/// warm; anything else, including a drop, releases the followers unmarked.
pub(crate) struct Lead {
    slots: Arc<Mutex<Slots>>,
    key: PrefixKey,
    id: u64,
    ended: bool,
}

impl Lead {
    /// Passes the leader's stream through untouched. Its first event marks
    /// the response as begun; an error or an end before any event releases
    /// the followers without the mark, and so does dropping the stream.
    pub(crate) fn watch(self, stream: ProviderStream) -> ProviderStream {
        Box::pin(LeadStream {
            stream,
            lead: Some(self),
        })
    }

    fn end(&mut self, begun: bool) {
        if std::mem::replace(&mut self.ended, true) {
            return;
        }
        let mut slots = self.slots.lock();
        if slots
            .map
            .get(&self.key)
            .is_none_or(|slot| slot.id != self.id)
        {
            // Replaced or evicted: its followers were released then.
            return;
        }
        let released = if begun {
            slots
                .map
                .get_mut(&self.key)
                .and_then(|slot| slot.open.take())
        } else {
            slots.map.remove(&self.key).and_then(|slot| slot.open)
        };
        drop(slots);
        drop(released);
        tracing::debug!(
            key = %self.key,
            begun,
            "prompt prefix leader: followers released"
        );
    }
}

impl Drop for Lead {
    fn drop(&mut self) {
        self.end(false);
    }
}

struct LeadStream {
    stream: ProviderStream,
    lead: Option<Lead>,
}

impl Stream for LeadStream {
    type Item = CoreResult<ProviderEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = self.stream.as_mut().poll_next(cx);
        if let Poll::Ready(item) = &next
            && let Some(mut lead) = self.lead.take()
        {
            lead.end(matches!(item, Some(Ok(_))));
        }
        next
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RequestParameters, ToolDefinition};
    use futures::{StreamExt as _, channel::mpsc};
    use serde_json::json;

    const LONG: Duration = Duration::from_secs(30);
    const SETTLE: Duration = Duration::from_millis(50);

    fn warmup() -> PrefixWarmup {
        PrefixWarmup::with_limits(LONG, LONG, CAPACITY)
    }

    fn key(name: &str) -> PrefixKey {
        PrefixKey::new("fixture", name, &request("prompt"))
    }

    fn request(prompt: &str) -> ProviderRequest {
        ProviderRequest {
            model: "claude-fixture".into(),
            system: "stable system".into(),
            messages: vec![crate::ProviderMessage::Text {
                role: "user".into(),
                content: prompt.into(),
            }],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: json!({"type":"object"}),
            }],
            reasoning_effort: None,
            output_schema: None,
            parameters: RequestParameters::default(),
        }
    }

    /// A provider stream the test feeds by hand.
    fn feed() -> (
        mpsc::UnboundedSender<CoreResult<ProviderEvent>>,
        ProviderStream,
    ) {
        let (sender, receiver) = mpsc::unbounded();
        (sender, Box::pin(receiver))
    }

    fn text() -> ProviderEvent {
        ProviderEvent::TextDelta("hello".into())
    }

    type Started = Arc<Mutex<Vec<&'static str>>>;

    /// One gated request in the background; `started` records when it may send.
    fn follower(
        warmup: &PrefixWarmup,
        key: PrefixKey,
        name: &'static str,
        started: &Started,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<CoreResult<()>> {
        let (warmup, started) = (warmup.clone(), Arc::clone(started));
        tokio::spawn(async move {
            let lead = warmup.enter(key, &cancel).await?;
            assert!(lead.is_none(), "{name} was expected to follow");
            started.lock().push(name);
            Ok(())
        })
    }

    async fn finished<T>(handle: tokio::task::JoinHandle<T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("request still held")
            .expect("request task")
    }

    async fn leads(warmup: &PrefixWarmup, key: PrefixKey) -> bool {
        warmup
            .enter(key, &CancellationToken::new())
            .await
            .unwrap()
            .is_some()
    }

    #[tokio::test]
    async fn followers_wait_until_the_leaders_response_begins_then_all_go() {
        let warmup = warmup();
        let started = Started::default();
        let lead = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("first request leads");
        let followers = ["f0", "f1", "f2"]
            .map(|name| follower(&warmup, key("a"), name, &started, CancellationToken::new()));
        tokio::time::sleep(SETTLE).await;
        assert!(started.lock().is_empty());
        let (sender, stream) = feed();
        let mut stream = lead.watch(stream);
        tokio::time::sleep(SETTLE).await;
        assert!(started.lock().is_empty(), "sending is not responding");
        sender.unbounded_send(Ok(text())).unwrap();
        assert!(matches!(
            stream.next().await,
            Some(Ok(ProviderEvent::TextDelta(_)))
        ));
        for handle in followers {
            finished(handle).await.unwrap();
        }
        assert_eq!(started.lock().len(), 3);
        // The mark outlives the leader's stream: later siblings go at once.
        drop(sender);
        assert!(stream.next().await.is_none());
        drop(stream);
        assert!(!leads(&warmup, key("a")).await);
        assert!(!leads(&warmup, key("a")).await);
    }

    #[tokio::test]
    async fn a_different_prefix_never_waits() {
        let warmup = warmup();
        let _lead = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads");
        assert!(leads(&warmup, key("b")).await);
    }

    #[tokio::test]
    async fn a_leader_whose_send_fails_releases_followers_without_a_warm_mark() {
        let warmup = warmup();
        let started = Started::default();
        let lead = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads");
        let waiting = follower(&warmup, key("a"), "f", &started, CancellationToken::new());
        tokio::time::sleep(SETTLE).await;
        assert!(!waiting.is_finished());
        // The runtime drops the lead when `stream()` returns an error.
        drop(lead);
        finished(waiting).await.unwrap();
        // No mark: the next request leads again and the one after it waits.
        let next = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads again");
        let waiting = follower(&warmup, key("a"), "g", &started, CancellationToken::new());
        tokio::time::sleep(SETTLE).await;
        assert!(!waiting.is_finished());
        drop(next);
        finished(waiting).await.unwrap();
    }

    #[tokio::test]
    async fn an_error_or_an_empty_end_or_a_dropped_stream_releases_without_a_mark() {
        for mode in ["error", "end", "drop"] {
            let warmup = warmup();
            let started = Started::default();
            let lead = warmup
                .enter(key("a"), &CancellationToken::new())
                .await
                .unwrap()
                .expect("leads");
            let (sender, stream) = feed();
            let mut stream = lead.watch(stream);
            let waiting = follower(&warmup, key("a"), "f", &started, CancellationToken::new());
            tokio::time::sleep(SETTLE).await;
            assert!(!waiting.is_finished(), "{mode}");
            match mode {
                "error" => {
                    sender
                        .unbounded_send(Err(CoreError::Provider("HTTP 529".into())))
                        .unwrap();
                    assert!(matches!(stream.next().await, Some(Err(_))));
                }
                "end" => {
                    drop(sender);
                    assert!(stream.next().await.is_none());
                }
                _ => drop(stream),
            }
            finished(waiting).await.unwrap();
            assert!(leads(&warmup, key("a")).await, "{mode} left a warm mark");
        }
    }

    #[tokio::test]
    async fn a_hung_leader_holds_followers_only_until_its_deadline_then_is_replaced() {
        let wait = Duration::from_millis(300);
        let warmup = PrefixWarmup::with_limits(wait, LONG, CAPACITY);
        let started = Started::default();
        let _hung = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads");
        let begun = Instant::now();
        finished(follower(
            &warmup,
            key("a"),
            "f",
            &started,
            CancellationToken::new(),
        ))
        .await
        .unwrap();
        assert!(begun.elapsed() >= Duration::from_millis(280));
        // Past the deadline the next arrival leads at once instead of waiting.
        let again = Instant::now();
        let replacement = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap();
        assert!(replacement.is_some());
        assert!(again.elapsed() < wait / 2);
        let waiting = follower(&warmup, key("a"), "g", &started, CancellationToken::new());
        tokio::time::sleep(SETTLE).await;
        assert!(!waiting.is_finished(), "the replacement leads");
        drop(replacement);
        finished(waiting).await.unwrap();
    }

    #[tokio::test]
    async fn a_followers_own_cancel_ends_its_wait() {
        let warmup = warmup();
        let started = Started::default();
        let _lead = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads");
        let cancel = CancellationToken::new();
        let waiting = follower(&warmup, key("a"), "f", &started, cancel.clone());
        tokio::time::sleep(SETTLE).await;
        cancel.cancel();
        assert!(matches!(finished(waiting).await, Err(CoreError::Cancelled)));
        assert!(started.lock().is_empty());
        // An already cancelled request never leads either.
        assert!(matches!(
            warmup.enter(key("b"), &cancel).await,
            Err(CoreError::Cancelled)
        ));
        assert!(leads(&warmup, key("b")).await);
    }

    #[tokio::test]
    async fn an_expired_leader_cannot_warm_or_clear_its_replacement() {
        let wait = Duration::from_millis(300);
        let warmup = PrefixWarmup::with_limits(wait, LONG, CAPACITY);
        let old = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads");
        tokio::time::sleep(wait + SETTLE).await;
        let next = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("an expired leader is replaced");
        // The old leader's response begins late: its mark must not land.
        let (sender, stream) = feed();
        let mut stream = old.watch(stream);
        sender.unbounded_send(Ok(text())).unwrap();
        assert!(stream.next().await.is_some());
        let started = Started::default();
        let waiting = follower(&warmup, key("a"), "f", &started, CancellationToken::new());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished(), "still waiting on the replacement");
        let (sender, stream) = feed();
        let mut stream = next.watch(stream);
        sender.unbounded_send(Ok(text())).unwrap();
        assert!(stream.next().await.is_some());
        finished(waiting).await.unwrap();
        assert!(!leads(&warmup, key("a")).await, "the replacement warmed it");
    }

    #[tokio::test]
    async fn a_warm_mark_expires() {
        let warm = Duration::from_millis(100);
        let warmup = PrefixWarmup::with_limits(LONG, warm, CAPACITY);
        let lead = warmup
            .enter(key("a"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads");
        let (sender, stream) = feed();
        let mut stream = lead.watch(stream);
        sender.unbounded_send(Ok(text())).unwrap();
        assert!(stream.next().await.is_some());
        assert!(!leads(&warmup, key("a")).await);
        tokio::time::sleep(warm + SETTLE).await;
        assert!(leads(&warmup, key("a")).await);
    }

    #[tokio::test]
    async fn evicting_a_pending_leader_releases_its_followers() {
        let warmup = PrefixWarmup::with_limits(LONG, LONG, 4);
        let started = Started::default();
        let _old = warmup
            .enter(key("old"), &CancellationToken::new())
            .await
            .unwrap()
            .expect("leads");
        let waiting = follower(&warmup, key("old"), "f", &started, CancellationToken::new());
        tokio::time::sleep(SETTLE).await;
        assert!(!waiting.is_finished());
        let mut newer = Vec::new();
        for index in 0..4 {
            newer.push(
                warmup
                    .enter(key(&format!("new-{index}")), &CancellationToken::new())
                    .await
                    .unwrap(),
            );
        }
        finished(waiting).await.unwrap();
        assert_eq!(warmup.slots.lock().map.len(), 4);
    }

    #[test]
    fn the_key_is_the_shared_prefix() {
        let base = PrefixKey::new("anthropic", "account-a", &request("one task"));
        // Siblings differ only in their messages and their own cache key.
        let mut sibling = request("another task");
        sibling.parameters.cache_key = Some("thread-2".into());
        assert_eq!(PrefixKey::new("anthropic", "account-a", &sibling), base);
        assert_ne!(
            PrefixKey::new("anthropic", "account-b", &request("one task")),
            base
        );
        assert_ne!(
            PrefixKey::new("router", "account-a", &request("one task")),
            base
        );
        let changes: [fn(&mut ProviderRequest); 6] = [
            |request| request.model = "claude-other".into(),
            |request| request.system.push_str(" changed"),
            |request| request.tools[0].description = "Read a file.".into(),
            |request| request.reasoning_effort = Some("high".into()),
            |request| request.output_schema = Some(json!({"type":"object"})),
            |request| {
                request
                    .parameters
                    .headers
                    .insert("anthropic-beta".into(), "fixture".into());
            },
        ];
        for change in changes {
            let mut other = request("one task");
            change(&mut other);
            assert_ne!(PrefixKey::new("anthropic", "account-a", &other), base);
        }
    }
}
