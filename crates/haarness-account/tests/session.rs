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

#[tokio::test]
async fn cloud_requests_preserve_flat_and_legacy_error_messages() {
    let app = Router::new()
        .route("/flat", post(|| async { (StatusCode::CONFLICT, Json(json!({"code":"invalid_state", "message":"This workspace is already running."}))) }))
        .route("/legacy", post(|| async { (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":{"message":"Try again later."}}))) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dir = std::env::temp_dir().join(format!("haarness-account-test-{}", uuid::Uuid::now_v7()));
    let config = Config::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        dir.clone(),
        "haarness-cli",
    )
    .unwrap();
    credentials::save(
        &config.path(),
        &Credentials {
            api_url: config.api_url.to_string(),
            access_token: "fixture-access".into(),
            refresh_token: "fixture-refresh".into(),
            access_expires_at: Utc::now() + Duration::minutes(10),
        },
    )
    .unwrap();
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let client = Client::new(config).unwrap();
    for (path, message) in [
        ("flat", "This workspace is already running."),
        ("legacy", "Try again later."),
    ] {
        assert_eq!(
            client
                .request(reqwest::Method::POST, path, Some(json!({})))
                .await
                .unwrap_err()
                .to_string(),
            message
        );
    }
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn explicit_api_key_is_account_bound_redacted_and_never_falls_back_to_oauth() {
    let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let state = revoked.clone();
    let app=Router::new().route("/api/v1/key",get(move |h:HeaderMap| {
        let state=state.clone();async move {
            assert_eq!(h["authorization"],"Bearer hck_test_unified");
            if state.load(Ordering::SeqCst) { return (StatusCode::UNAUTHORIZED,Json(json!({"message":"revoked"}))); }
            (StatusCode::OK,Json(json!({"key":{"id":"key-id","accountId":"account-one","label":"test","scopes":["models:read","models:invoke"],"expiresAt":Utc::now()+Duration::hours(1)},"accountName":"Test","routerUrl":"http://127.0.0.1:19082/v1"})))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dir = std::env::temp_dir().join(format!("haarness-key-test-{}", uuid::Uuid::now_v7()));
    let config = Config::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        dir.clone(),
        "haarness-cli",
    )
    .unwrap()
    .with_api_key("hck_test_unified".into())
    .unwrap();
    assert!(!format!("{config:?}").contains("hck_test_unified"));
    let stored = Credentials {
        api_url: config.api_url.to_string(),
        access_token: "old-oauth".into(),
        refresh_token: "old-refresh".into(),
        access_expires_at: Utc::now() - Duration::hours(1),
    };
    credentials::save(&config.path(), &stored).unwrap();
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let client = Client::new(config.clone()).unwrap();
    assert_eq!(
        client.status().await.unwrap()["defaultAccountId"],
        "account-one"
    );
    assert!(client.select_account("another-account").await.is_err());
    let inference = client.router_session().await.unwrap();
    assert_eq!(inference.token, "hck_test_unified");
    assert!(inference.expires_at <= Utc::now() + Duration::minutes(5));
    assert!(client.logout().await.is_err());
    revoked.store(true, Ordering::SeqCst);
    assert!(client.status().await.is_err());
    assert!(client.router_session().await.is_err());
    assert_eq!(
        credentials::load(&config.path())
            .unwrap()
            .unwrap()
            .refresh_token,
        "old-refresh"
    );
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_renewal_cloud_cannot_answer_is_transient_and_a_refused_one_is_not() {
    use axum::response::IntoResponse as _;
    use haarness_account::Error;
    let answers = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/oauth/token",
            post(|State(answers): State<Arc<AtomicUsize>>| async move {
                match answers.fetch_add(1, Ordering::SeqCst) {
                    // A load balancer's page while Cloud restarts.
                    0 => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        [("content-type", "text/html")],
                        "<html><body>503 Service Temporarily Unavailable</body></html>",
                    )
                        .into_response(),
                    1 => (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"error": "rate_limited"})),
                    )
                        .into_response(),
                    2 => Json(json!({"access_token": "renewed", "refresh_token": "next", "expires_in": 600}))
                        .into_response(),
                    _ => (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "invalid_grant"})),
                    )
                        .into_response(),
                }
            }),
        )
        .with_state(answers.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let offline_url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let dir = std::env::temp_dir().join(format!("haarness-account-test-{}", uuid::Uuid::now_v7()));
    let expired = |config: &Config| {
        credentials::save(
            &config.path(),
            &Credentials {
                api_url: config.api_url.to_string(),
                access_token: "expired".into(),
                refresh_token: "refresh".into(),
                access_expires_at: Utc::now() - Duration::minutes(5),
            },
        )
        .unwrap();
    };
    let config = Config::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        dir.join("online"),
        "haarness-cli",
    )
    .unwrap();
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    expired(&config);
    let client = Client::new(config.clone()).unwrap();
    for status in [503, 429] {
        let error = client.bearer(None).await.unwrap_err();
        assert!(
            matches!(&error, Error::Api { status: got, .. } if *got == status)
                && error.is_transient(),
            "{error:?}"
        );
        // A passing outage keeps the sign-in for the next attempt.
        assert!(credentials::load(&config.path()).unwrap().is_some());
    }
    // Nothing listening at all.
    let offline = Config::new(&offline_url, dir.join("offline"), "haarness-cli").unwrap();
    expired(&offline);
    let error = Client::new(offline)
        .unwrap()
        .bearer(None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Http(_)) && error.is_transient(),
        "{error:?}"
    );
    // Back: the renewal goes through.
    assert_eq!(client.bearer(None).await.unwrap(), "renewed");
    // A revoked refresh token is no outage.
    let error = client.bearer(Some("renewed")).await.unwrap_err();
    assert!(
        matches!(error, Error::NotLoggedIn) && !error.is_transient(),
        "{error:?}"
    );
    assert!(!Error::Other("authorization_pending".into()).is_transient());
    assert!(
        !Error::Api {
            status: 401,
            message: String::new()
        }
        .is_transient()
    );
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn both_sign_in_flows_name_this_computer_in_their_token_request() {
    let forms = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route(
            "/oauth/device_authorization",
            post(|| async {
                Json(json!({"device_code": "dc-1", "user_code": "ABCD-EFGH",
                    "verification_uri_complete": "http://127.0.0.1/device?code=ABCD-EFGH",
                    "expires_in": 60, "interval": 1}))
            }),
        )
        .route(
            "/oauth/token",
            post(
                |State(forms): State<Arc<std::sync::Mutex<Vec<String>>>>, body: String| async move {
                    forms.lock().unwrap().push(body);
                    Json(json!({"access_token": "access", "refresh_token": "refresh", "expires_in": 600}))
                },
            ),
        )
        .with_state(forms.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dir = std::env::temp_dir().join(format!("haarness-account-test-{}", uuid::Uuid::now_v7()));
    let config = Config::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        dir.clone(),
        "haarness-cli",
    )
    .unwrap();
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let client = Client::new(config.clone()).unwrap();

    client
        .begin_login(true)
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();

    let login = client.begin_login(false).await.unwrap();
    let url = url::Url::parse(&login.url).unwrap();
    let query = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .unwrap()
            .1
            .into_owned()
    };
    let (redirect, state) = (query("redirect_uri"), query("state"));
    let finished = tokio::spawn(login.finish());
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("{redirect}?code=hcc_1&state={state}"))
        .send()
        .await
        .unwrap();
    finished.await.unwrap().unwrap();

    let label = haarness_account::device_label(false, &haarness_account::computer_name());
    assert!(label.starts_with("Haarness CLI on "), "{label}");
    let forms = forms.lock().unwrap().clone();
    let fields = |form: &str| -> std::collections::HashMap<String, String> {
        url::form_urlencoded::parse(form.as_bytes())
            .into_owned()
            .collect()
    };
    assert_eq!(forms.len(), 2, "{forms:?}");
    let device = fields(&forms[0]);
    assert_eq!(
        device["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(device["device_label"], label);
    let browser = fields(&forms[1]);
    assert_eq!(browser["grant_type"], "authorization_code");
    assert_eq!(browser["device_label"], label);
    assert!(credentials::load(&config.path()).unwrap().is_some());
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}
