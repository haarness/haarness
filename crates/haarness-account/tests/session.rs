use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::{Duration, Utc};
use haarness_account::{Client, Config, Credentials, credentials};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn independent_clients_rotate_once_and_logout_invalidates_disk() {
    let rotations = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/oauth/token", post(|State(n): State<Arc<AtomicUsize>>| async move {
        n.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Json(json!({"access_token":"new-access", "refresh_token":"new-refresh", "expires_in":600}))
    })).route("/api/v1/me", get(|h: HeaderMap| async move {
        if h.get("authorization").unwrap() == "Bearer new-access" {(StatusCode::OK, Json(json!({"user":{}, "accounts":[], "defaultAccountId":"a"})))}
        else {(StatusCode::UNAUTHORIZED, Json(json!({})))}
    })).route("/oauth/revoke", post(|| async { Json(json!({})) })).with_state(rotations.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dir = std::env::temp_dir().join(format!("haarness-account-test-{}", uuid::Uuid::now_v7()));
    let config = Config::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        dir.clone(),
        "haarness-cli",
    )
    .unwrap();
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let old = Credentials {
        api_url: config.api_url.to_string(),
        access_token: "old-access".into(),
        refresh_token: "old-refresh".into(),
        access_expires_at: Utc::now() + Duration::minutes(10),
    };
    credentials::save(&config.path(), &old).unwrap();
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let c = Client::new(config.clone()).unwrap();
        calls.spawn(async move { c.status().await.unwrap() });
    }
    while let Some(result) = calls.join_next().await {
        assert_eq!(result.unwrap()["loggedIn"], true);
    }
    assert_eq!(rotations.load(Ordering::SeqCst), 1);
    let current = credentials::load(&config.path()).unwrap().unwrap();
    assert_eq!(current.refresh_token, "new-refresh");
    let client = Client::new(config.clone()).unwrap();
    client.logout().await.unwrap();
    assert_eq!(client.status().await.unwrap()["loggedIn"], false);
    assert!(client.bearer(None).await.is_err());
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn another_origin_is_never_contacted() {
    let dir = std::env::temp_dir().join(format!("haarness-account-test-{}", uuid::Uuid::now_v7()));
    let config = Config::new("http://127.0.0.1:1", dir.clone(), "haarness-cli").unwrap();
    let c = Credentials {
        api_url: "https://another.example/".into(),
        access_token: "secret".into(),
        refresh_token: "refresh".into(),
        access_expires_at: Utc::now() - Duration::minutes(1),
    };
    credentials::save(&config.path(), &c).unwrap();
    let client = Client::new(config.clone()).unwrap();
    let e = client.bearer(None).await.unwrap_err().to_string();
    assert!(e.contains("another Cloud service"));
    assert!(credentials::load(&config.path()).unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reject_untrusted_token_destinations() {
    for url in [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com?secret=x",
        "file:///tmp/token",
    ] {
        assert!(haarness_account::safe_url(url).is_err());
    }
    assert!(haarness_account::safe_url("http://127.0.0.1:18080").is_ok());
}
