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

/// How long starting up, and a catalog read, wait for lists still loading.
pub const WAIT: Duration = Duration::from_secs(3);

/// The registries (by address) whose lists outlasted a wait and are still
/// loading. A read meanwhile answers at once: it would only wait for the
/// same lists, which announce themselves when they come.
static LATE: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());

fn key(registry: &Arc<dyn ProviderRegistry>) -> usize {
    Arc::as_ptr(registry).cast::<()>() as usize
}

/// The provider catalog, once every signed-in provider's list is refreshed
/// (`force`: even one fetched moments ago). Waits for the lists at most
/// `wait`, or for as long as they take with `None`. Lists that take longer
/// keep loading; once they are all in, `assets.updated` (`resource:
/// "provider"`) tells clients to read the catalog again if it changed, so a
/// model picker already open fills in by itself.
pub async fn catalog(
    runtime: &Arc<AgentRuntime>,
    force: bool,
    wait: Option<Duration>,
) -> Vec<ProviderInfo> {
    let registry = runtime.providers();
    if wait.is_some() && LATE.lock().contains(&key(&registry)) {
        return registry.list();
    }
    let refreshes: Vec<_> = registry
        .list()
        .iter()
        .filter(|info| info.authenticated)
        .filter_map(|info| registry.get(&info.id))
        .map(|provider| tokio::spawn(async move { provider.refresh_models(force).await }))
        .collect();
    let mut all = tokio::spawn(futures::future::join_all(refreshes));
    let Some(wait) = wait else {
        let _ = all.await;
        return registry.list();
    };
    if tokio::time::timeout(wait, &mut all).await.is_ok() {
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
            catalog(&runtime, false, Some(Duration::from_millis(200))),
        )
        .await
        .expect("a read waited for the slow list");
        assert_eq!(model_ids(&shown), ["built-in"]);
        // Reads while the list is still loading answer at once.
        let again = tokio::time::timeout(
            Duration::from_millis(100),
            catalog(&runtime, false, Some(WAIT)),
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
            model_ids(&catalog(&runtime, false, Some(Duration::from_millis(100))).await),
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
            async move { catalog(&runtime, true, None).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!refresh.is_finished());
        provider.answer.notify_one();
        assert_eq!(model_ids(&refresh.await.unwrap()), ["built-in", "live"]);
    }
}
