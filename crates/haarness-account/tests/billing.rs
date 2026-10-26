use axum::{
    Json, Router,
    extract::{Query, State, WebSocketUpgrade, ws::Message},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use chrono::{Duration, Utc};
use haarness_account::{Client, Config, Credentials, billing, credentials};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration as Wait,
};
use tokio::sync::{broadcast, watch};

const A: &str = "01930000-0000-7000-8000-000000000001";
const B: &str = "01930000-0000-7000-8000-000000000002";
#[derive(Clone)]
struct Data {
    events: broadcast::Sender<Value>,
    requests: Arc<AtomicUsize>,
    balance: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}
struct Fixture {
    client: Client,
    data: Data,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.client.config.config_dir);
    }
}
async fn fixture() -> Fixture {
    let data = Data {
        events: broadcast::channel(32).0,
        requests: Arc::new(AtomicUsize::new(0)),
        balance: Arc::new(AtomicUsize::new(23)),
        fail: Arc::new(AtomicBool::new(false)),
    };
    let app = Router::new()
        .route("/api/v1/me", get(|| async { Json(json!({"defaultAccountId":A,"accounts":[{"id":A,"name":"Personal"},{"id":B,"name":"Team"}]})) }))
        .route("/api/v1/billing/wallet", get(|State(d): State<Data>, Query(q): Query<HashMap<String,String>>| async move {
            d.requests.fetch_add(1, Ordering::SeqCst);
            Json(json!({"status":"active","includedAi":{"credits":if q.get("accountId").map(String::as_str)==Some(B) {"100".into()} else {format!("{}.123456", d.balance.load(Ordering::SeqCst))},"nanos":if q.get("accountId").map(String::as_str)==Some(B) {"100000000".into()} else {format!("{}123456", d.balance.load(Ordering::SeqCst))}},"purchased":{"credits":"9007199254740993.000001","nanos":"9007199254740993000001"},"heldReserve":{"credits":"4.5"},"cloudUnitSecondsAvailable":7200,"cloudUnitSecondsReserved":360,"overageOptIn":false,"resetsAt":"2026-10-01T00:00:00Z","trialAvailable":q.get("accountId").map(String::as_str)==Some(B),"autoTopup":{"enabled":true},"aiSpendLast7Days":{"nanos":"23123456"}}))
        }))
        .route("/api/v1/billing/subscription", get(|State(d): State<Data>, Query(q): Query<HashMap<String,String>>| async move {
            if d.fail.load(Ordering::SeqCst) { return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message":"temporarily unavailable"}))); }
            if q.get("accountId").map(String::as_str)==Some(B) { return (StatusCode::NOT_FOUND,Json(json!({}))); }
            (StatusCode::OK,Json(json!({"planCode":"power","status":"active","cancelAtPeriodEnd":true,"currentPeriodEnd":"2026-10-01T00:00:00Z","seats":3,"billingInterval":"year"})))
        }))
        .route("/api/v1/events", get(|State(d): State<Data>, ws: WebSocketUpgrade| async move {
            ws.on_upgrade(move |mut socket| async move {
                let mut events = d.events.subscribe();
                socket.send(Message::Text(json!({"type":"ready"}).to_string().into())).await.unwrap();
                loop { tokio::select! {
                    Ok(event) = events.recv() => {
                        if event["type"] == "disconnect" { break; }
                        if socket.send(Message::Text(event.to_string().into())).await.is_err() { break; }
                    },
                    message = socket.recv() => { if message.is_none() || matches!(message, Some(Ok(Message::Close(_)))) { break; } }
                }}
            }).into_response()
        })).with_state(data.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = Config::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        std::env::temp_dir().join(format!("haarness-billing-test-{}", uuid::Uuid::now_v7())),
        "haarness-cli",
    )
    .unwrap();
    credentials::save(
        &config.path(),
        &Credentials {
            api_url: config.api_url.to_string(),
            access_token: "fixture".into(),
            refresh_token: "fixture-refresh".into(),
            access_expires_at: Utc::now() + Duration::minutes(10),
        },
    )
    .unwrap();
    Fixture {
        client: Client::new(config).unwrap(),
        data,
        server: tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    }
}
async fn until(
    receiver: &mut watch::Receiver<billing::State>,
    check: impl Fn(&billing::State) -> bool,
) -> billing::State {
    tokio::time::timeout(Wait::from_secs(6), async {
        loop {
            let state = receiver.borrow_and_update().clone();
            if check(&state) {
                return state;
            }
            receiver.changed().await.unwrap();
        }
    })
    .await
    .expect("billing update timed out")
}

async fn state_lines(f: &Fixture) -> Vec<String> {
    f.client.billing_summary().await.unwrap().lines()
}

#[tokio::test]
async fn snapshot_preserves_precision_holds_and_no_plan_without_hiding_errors() {
    let f = fixture().await;
    let state = f.client.billing_summary().await.unwrap();
    let s = state.summary.unwrap();
    assert_eq!(s.included_credits, "23.123456");
    assert_eq!(s.purchased_credits, "9007199254740993.000001");
    assert_eq!(s.held_credits, "4.5");
    assert_eq!(s.cloud_units, "2.00");
    assert_eq!(s.cloud_units_held, "0.10");
    assert!(s.cancel_at_period_end);
    assert!(s.billing_url.ends_with(&format!("?accountId={A}")));
    // A yearly three-seat plan with automatic top-ups: 23.123456 credits at 23.123456 a week last 7 days.
    assert_eq!(s.seats, Some(3));
    assert_eq!(s.billing_interval.as_deref(), Some("year"));
    assert!(s.auto_topup && !s.trial && !s.trial_available && !s.out_of_credits);
    assert_eq!(s.days_left, Some(7));
    let lines = state_lines(&f).await;
    assert!(
        lines.iter().any(|line| line.contains("yearly · 3 seats")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line == "Auto top-up      on"),
        "{lines:?}"
    );
    f.client.select_account(B).await.unwrap();
    let b = f.client.billing_summary().await.unwrap().summary.unwrap();
    assert_eq!(b.plan, "No plan");
    assert!(
        b.trial_available && !b.out_of_credits,
        "no plan is not 'out of credits'"
    );
    f.data.fail.store(true, Ordering::SeqCst);
    assert!(
        f.client
            .billing_summary()
            .await
            .unwrap_err()
            .to_string()
            .contains("temporarily unavailable")
    );
}

#[tokio::test]
async fn websocket_balances_reconnect_switch_logout_and_stale_errors() {
    let f = fixture().await;
    let mut watcher = billing::watch(f.client.clone());
    until(&mut watcher.updates, |s| s.summary.is_some()).await;
    // Wait for ready-triggered reconciliation before checking ignored events.
    tokio::time::sleep(Wait::from_millis(350)).await;
    let before = f.data.requests.load(Ordering::SeqCst);
    f.data
        .events
        .send(json!({"type":"entity.changed","entity":"billing","accountId":B}))
        .unwrap();
    f.data
        .events
        .send(json!({"type":"entity.changed","entity":"workspace","accountId":A}))
        .unwrap();
    tokio::time::sleep(Wait::from_millis(250)).await;
    assert_eq!(before, f.data.requests.load(Ordering::SeqCst));
    f.data.balance.store(19, Ordering::SeqCst);
    f.data
        .events
        .send(json!({"type":"entity.changed","entity":"billing","accountId":A}))
        .unwrap();
    until(&mut watcher.updates, |s| {
        s.summary
            .as_ref()
            .is_some_and(|s| s.included_credits == "19.123456")
    })
    .await;
    f.data.balance.store(17, Ordering::SeqCst);
    f.data.events.send(json!({"type":"disconnect"})).unwrap();
    until(&mut watcher.updates, |s| {
        s.summary
            .as_ref()
            .is_some_and(|s| s.included_credits == "17.123456")
    })
    .await;
    tokio::time::timeout(Wait::from_secs(4), async {
        while f.data.events.receiver_count() == 0 {
            tokio::time::sleep(Wait::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.data.fail.store(true, Ordering::SeqCst);
    f.data
        .events
        .send(json!({"type":"resync_required"}))
        .unwrap();
    let stale = until(&mut watcher.updates, |s| s.error.is_some()).await;
    assert_eq!(stale.summary.unwrap().included_credits, "17.123456");
    f.data.fail.store(false, Ordering::SeqCst);
    // Selecting another account clears the old snapshot before its HTTP result.
    f.client.select_account(B).await.unwrap();
    let state = until(&mut watcher.updates, |s| {
        s.summary.as_ref().is_some_and(|s| s.account_id == B)
    })
    .await;
    assert_eq!(state.summary.unwrap().included_credits, "100");
    assert!(state.error.is_none());
    credentials::delete(&f.client.config.path()).unwrap();
    let state = until(&mut watcher.updates, |s| !s.logged_in).await;
    assert!(state.summary.is_none());
}

#[test]
fn readable_credit_balances_never_round_up_or_lose_large_integer_precision() {
    for (raw, display) in [
        ("24996.302072", "24,996.3"),
        ("60000.000000", "60,000"),
        ("9007199254740993.123456", "9,007,199,254,740,993.12"),
        ("0.000001", "<0.01"),
        ("0000.000000", "0"),
        ("nan", "—"),
    ] {
        assert_eq!(billing::display_credits(raw), display);
    }
}

#[test]
fn days_left_counts_purchased_credits_only_when_they_can_pay() {
    assert_eq!(billing::days_left(7_000, 7_000, false, 7_000), Some(7));
    assert_eq!(billing::days_left(7_000, 7_000, true, 7_000), Some(14));
    assert_eq!(billing::days_left(7_000, 0, false, 0), None);
}
