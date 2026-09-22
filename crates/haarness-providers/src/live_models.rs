//! Last successful live model list. Failed/partial requests never poison it.
use haarness_protocol::ModelInfo;
use std::{
    future::Future,
    sync::RwLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Default)]
pub(crate) struct LiveModels {
    models: RwLock<Option<Vec<ModelInfo>>>,
    checked: Mutex<Option<Instant>>,
}
impl LiveModels {
    pub fn get(&self, fallback: &[ModelInfo]) -> Vec<ModelInfo> {
        self.models
            .read()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap_or_else(|| fallback.to_vec())
    }
    pub async fn refresh<F, Fut>(&self, force: bool, fetch: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<Vec<ModelInfo>>>,
    {
        let requested = Instant::now();
        let mut checked = self.checked.lock().await;
        if checked.is_some_and(|at| {
            at >= requested
                || at.elapsed()
                    < if force {
                        Duration::from_secs(1)
                    } else {
                        Duration::from_secs(300)
                    }
        }) {
            return;
        }
        // Bound a whole paginated request, including body reads.
        if let Ok(Some(models)) = tokio::time::timeout(Duration::from_secs(10), fetch()).await {
            *self.models.write().unwrap() = Some(models);
        }
        *checked = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: false,
            reasoning_efforts: vec![],
            images: false,
            video: false,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        }
    }
    #[tokio::test]
    async fn live_membership_is_authoritative_and_failures_keep_last_success() {
        let cache = LiveModels::default();
        let fallback = vec![model("static")];
        assert_eq!(cache.get(&fallback)[0].id, "static");
        cache
            .refresh(true, || async { Some(vec![model("live")]) })
            .await;
        assert_eq!(cache.get(&fallback)[0].id, "live");
        *cache.checked.lock().await = None;
        cache.refresh(true, || async { None }).await;
        assert_eq!(cache.get(&fallback)[0].id, "live");
        *cache.checked.lock().await = None;
        cache.refresh(true, || async { Some(vec![]) }).await;
        assert!(
            cache.get(&fallback).is_empty(),
            "an authoritative empty response cannot resurrect static models"
        );
    }
    #[tokio::test]
    async fn refreshes_are_coalesced_and_reads_use_the_cache() {
        let cache = LiveModels::default();
        let calls = AtomicUsize::new(0);
        let requests = (0..10).map(|_| {
            cache.refresh(true, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                Some(vec![model("live")])
            })
        });
        futures::future::join_all(requests).await;
        cache
            .refresh(false, || async { panic!("ordinary read should use cache") })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        *cache.checked.lock().await = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(301))
                .unwrap(),
        );
        cache
            .refresh(false, || async { Some(vec![model("next")]) })
            .await;
        assert_eq!(cache.get(&[])[0].id, "next");
    }
}
