use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use haarness_account::{Client, Config};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

#[tokio::test]
async fn router_selection_persists_isolates_keys_and_deleted_selection_fails_closed() {
    let id = Uuid::now_v7().to_string();
    let account = Uuid::now_v7().to_string();
    let deleted = Arc::new(AtomicBool::new(false));
    let row = json!({"id":id,"accountId":account,"name":"Own credentials","routerUrl":format!("http://127.0.0.1:19082/routers/{id}/v1")});
    let list = row.clone();
    let single = row.clone();
    let app = Router::new()
        .route(
            "/api/v1/routers",
            get(move || {
                let row = list.clone();
                async move { Json(json!({"items":[row]})) }
            }),
        )
        .route(
            "/api/v1/routers/{id}",
            get(
                move |Path(_): Path<String>, State(deleted): State<Arc<AtomicBool>>| {
                    let row = single.clone();
                    async move {
                        if deleted.load(Ordering::SeqCst) {
                            (
                                StatusCode::NOT_FOUND,
                                Json(json!({"message":"Router unavailable"})),
                            )
                        } else {
                            (StatusCode::OK, Json(row))
                        }
                    }
                },
            ),
        )
        .route(
            "/api/v1/key",
            get(|| async { Json(json!({"routerUrl":"http://127.0.0.1:19082/v1","key":{}})) }),
        )
        .with_state(deleted.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let directory = std::env::temp_dir().join(format!("haarness-router-test-{}", Uuid::now_v7()));
    let config = Config::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        directory.clone(),
        "test",
    )
    .unwrap()
    .with_api_key("hck_test_one".into())
    .unwrap();
    let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let client = Client::new(config.clone()).unwrap();
    assert!(config.selected_router().unwrap().is_none());
    assert!(client.select_router("bad/id").await.is_err());
    assert!(
        client
            .select_router(&Uuid::now_v7().to_string())
            .await
            .is_err()
    );
    client.select_router(&id).await.unwrap();
    assert_eq!(config.selected_router().unwrap(), Some(id.clone()));
    assert_eq!(
        Client::new(config.clone())
            .unwrap()
            .router_session()
            .await
            .unwrap()
            .router_url,
        row["routerUrl"]
    );
    let other = config.clone().with_api_key("hck_test_two".into()).unwrap();
    assert!(other.selected_router().unwrap().is_none());
    let serialized = std::fs::read_to_string(directory.join("routers.json")).unwrap();
    assert!(!serialized.contains("hck_test_one"));
    deleted.store(true, Ordering::SeqCst);
    let error = match client.router_session().await {
        Ok(_) => panic!("deleted selection fell back"),
        Err(e) => e,
    };
    assert!(error.to_string().contains("Router unavailable"));
    std::fs::write(directory.join("routers.json"), "broken").unwrap();
    assert!(
        config.selected_router().is_err(),
        "corruption cannot silently select a billed default"
    );
    std::fs::write(directory.join("routers.json"), "[]").unwrap();
    assert!(config.selected_router().is_err());
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
