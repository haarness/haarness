//! Live model lists, read without anyone waiting long on a slow provider.
//!
//! Each signed-in provider's list comes from its own API, which may take up
//! to 10 s to answer or give up. The server once waited for every list before
//! it listened, and again on a catalog read once its copy had aged, so on a
//! stalled network (a captive portal, a VPN coming up) the terminal UI drew
//! nothing for as long, or its launcher gave up after 8 s.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use haarness_core::{AgentRuntime, ProviderRegistry};
use haarness_protocol::ProviderInfo;
use parking_lot::Mutex;
use serde_json::json;

/// How long starting up, and an ordinary read, wait for lists still loading.
pub const WAIT: Duration = Duration::from_secs(3);

/// How long a catalog read waits for the model lists still loading.
#[derive(Clone, Copy, Debug)]
pub enum Wait {
    /// An explicit refresh (a picker opening, a key just saved): for every
    /// list, even one fetched moments ago.
    All,
    /// An ordinary read: up to the time given, and for as long as it takes
    /// (each list gives up within 10 s) for a signed-in provider with no
    /// model to show yet, such as Haarness Cloud or an endpoint that lists
    /// its models only when asked: without its list a chat would start on a
    /// model nobody chose, or find no model at all.
    Read(Duration),
    /// The server starting: up to the time given, never longer, so it
    /// starts answering.
    Start(Duration),
}

/// The registries (by address) whose lists outlasted a wait and are still
/// loading. A read meanwhile answers at once, unless it has to wait for a
/// provider with nothing to show: it would only wait for the same lists,
/// which announce themselves when they come.
static LATE: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());

fn key(registry: &Arc<dyn ProviderRegistry>) -> usize {
    Arc::as_ptr(registry).cast::<()>() as usize
}

/// The provider catalog, once every signed-in provider's list is refreshed,
/// waiting for the lists as `wait` says. Lists that take longer keep
/// loading; once they are all in, `assets.updated` (`resource: "provider"`)
/// tells clients to read the catalog again if it changed, so a model picker
/// already open fills in by itself.
pub async fn catalog(runtime: &Arc<AgentRuntime>, wait: Wait) -> Vec<ProviderInfo> {
    let registry = runtime.providers();
    let listed = registry.list();
    // Signed in with nothing to show yet: the reader needs that list.
    let needed: BTreeSet<String> = listed
        .iter()
        .filter(|info| {
            matches!(wait, Wait::Read(_)) && info.authenticated && info.models.is_empty()
        })
        .map(|info| info.id.clone())
        .collect();
    if needed.is_empty() && !matches!(wait, Wait::All) && LATE.lock().contains(&key(&registry)) {
        return listed;
    }
    let force = matches!(wait, Wait::All);
    let started = tokio::time::Instant::now();
    let (mut first, mut rest) = (Vec::new(), Vec::new());
    for info in listed.iter().filter(|info| info.authenticated) {
        let Some(provider) = registry.get(&info.id) else {
            continue;
        };
        let refresh = tokio::spawn(async move { provider.refresh_models(force).await });
        if needed.contains(&info.id) {
            first.push(refresh);
        } else {
            rest.push(refresh);
        }
    }
    let _ = futures::future::join_all(first).await;
    let mut all = tokio::spawn(futures::future::join_all(rest));
    let limit = match wait {
        Wait::All => {
            let _ = all.await;
            return registry.list();
        }
        Wait::Read(limit) | Wait::Start(limit) => limit.saturating_sub(started.elapsed()),
    };
    if tokio::time::timeout(limit, &mut all).await.is_ok() {
        return registry.list();
    }
    let shown = registry.list();
    if !LATE.lock().insert(key(&registry)) {
        // Another read already waits for them.
        return shown;
    }
    let before = serde_json::to_value(&shown).ok();
    let events = runtime.events().clone();
    tokio::spawn(async move {
        let _ = all.await;
        LATE.lock().remove(&key(&registry));
        if serde_json::to_value(registry.list()).ok() != before {
            events.publish(
                "assets.updated",
                None,
                None,
                None,
                json!({"resource":"provider","action":"models"}),
            );
        }
    });
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use haarness_core::{
        CoreResult, EmptyToolRegistry, EventBus, ModelProvider, ProviderRequest, ProviderStream,
        StaticProviderRegistry,
    };
    use haarness_protocol::ModelInfo;
    use tokio_util::sync::CancellationToken;

    /// A provider whose live list comes only once the test lets it.
    struct SlowList {
        models: parking_lot::Mutex<Vec<&'static str>>,
        answer: tokio::sync::Notify,
    }

    #[async_trait]
    impl ModelProvider for SlowList {
        fn id(&self) -> &str {
            "slow"
        }
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "slow".into(),
                name: "Slow".into(),
                authenticated: true,
                models: self
                    .models
                    .lock()
                    .iter()
                    .map(|id| ModelInfo {
                        id: (*id).into(),
                        display_name: (*id).into(),
                        reasoning: false,
                        reasoning_efforts: Vec::new(),
                        images: false,
                        video: false,
                        tools: true,
                        context_window: None,
                        max_output_tokens: None,
                        pricing: None,
                    })
                    .collect(),
            }
        }
        async fn refresh_models(&self, _force: bool) {
            self.answer.notified().await;
            *self.models.lock() = vec!["built-in", "live"];
        }
        async fn stream(
            &self,
            _request: ProviderRequest,
            _cancel: CancellationToken,
        ) -> CoreResult<ProviderStream> {
            unreachable!("no turn runs here")
        }
    }

    fn runtime(provider: Arc<SlowList>) -> Arc<AgentRuntime> {
        Arc::new(AgentRuntime::new(
            Arc::new(haarness_store::SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new([
                provider as Arc<dyn ModelProvider>
            ])),
            Arc::new(EmptyToolRegistry),
            EventBus::new(64),
        ))
    }

    fn model_ids(catalog: &[ProviderInfo]) -> Vec<String> {
        catalog
            .iter()
            .flat_map(|info| info.models.iter().map(|model| model.id.clone()))
            .collect()
    }

    #[tokio::test]
    async fn a_slow_list_answers_with_what_is_there_and_announces_itself_when_it_comes() {
        let provider = Arc::new(SlowList {
            models: parking_lot::Mutex::new(vec!["built-in"]),
            answer: tokio::sync::Notify::new(),
        });
        let runtime = runtime(provider.clone());
        let mut events = runtime.events().subscribe(None).receiver;
        let shown = tokio::time::timeout(
            Duration::from_secs(2),
            catalog(&runtime, Wait::Read(Duration::from_millis(200))),
        )
        .await
        .expect("a read waited for the slow list");
        assert_eq!(model_ids(&shown), ["built-in"]);
        // Reads while the list is still loading answer at once.
        let again = tokio::time::timeout(
            Duration::from_millis(100),
            catalog(&runtime, Wait::Read(WAIT)),
        )
        .await
        .expect("a second read waited for the same list");
        assert_eq!(model_ids(&again), ["built-in"]);
        assert!(events.try_recv().is_err(), "nothing changed yet");

        provider.answer.notify_one();
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("the list announced itself")
            .unwrap();
        assert_eq!(event.event_type, "assets.updated");
        assert_eq!(event.payload["resource"], "provider");
        assert!(events.try_recv().is_err(), "announced once");
        // The next read has it.
        assert_eq!(
            model_ids(&catalog(&runtime, Wait::Read(Duration::from_millis(100))).await),
            ["built-in", "live"]
        );
    }

    #[tokio::test]
    async fn an_explicit_refresh_waits_for_the_list() {
        let provider = Arc::new(SlowList {
            models: parking_lot::Mutex::new(vec!["built-in"]),
            answer: tokio::sync::Notify::new(),
        });
        let runtime = runtime(provider.clone());
        let refresh = tokio::spawn({
            let runtime = runtime.clone();
            async move { catalog(&runtime, Wait::All).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!refresh.is_finished());
        provider.answer.notify_one();
        assert_eq!(model_ids(&refresh.await.unwrap()), ["built-in", "live"]);
    }

    /// Haarness Cloud, or an endpoint that lists its models only when
    /// asked, has no built-in list: an ordinary read that answered before
    /// its list came left `haarness run` with no model, or a new chat on a
    /// provider nobody chose. Starting up still does not wait for it.
    #[tokio::test]
    async fn a_read_waits_for_a_signed_in_provider_with_nothing_to_show_and_starting_does_not() {
        let provider = Arc::new(SlowList {
            models: parking_lot::Mutex::new(Vec::new()),
            answer: tokio::sync::Notify::new(),
        });
        let runtime = runtime(provider.clone());
        let started = tokio::time::timeout(
            Duration::from_secs(2),
            catalog(&runtime, Wait::Start(Duration::from_millis(100))),
        )
        .await
        .expect("starting up waited for the list");
        assert!(model_ids(&started).is_empty());
        let read = tokio::spawn({
            let runtime = runtime.clone();
            async move { catalog(&runtime, Wait::Read(Duration::from_millis(100))).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!read.is_finished(), "the read answered without the list");
        provider.answer.notify_waiters();
        assert_eq!(model_ids(&read.await.unwrap()), ["built-in", "live"]);
    }
}
