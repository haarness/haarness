//! Account-owned billing snapshots for native clients. Credentials stay here;
//! renderers receive only balances. WebSocket events invalidate HTTP snapshots.
use crate::{Client, Error, Result};
use futures::{SinkExt, StreamExt};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::{sync::watch, task::JoinHandle};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub logged_in: bool,
    pub summary: Option<Summary>,
    /// A retained snapshot is stale whenever an error is present.
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub account_id: String,
    pub account_name: String,
    pub plan: String,
    pub subscription_status: Option<String>,
    pub account_status: String,
    pub included_credits: String,
    pub purchased_credits: String,
    pub held_credits: String,
    pub cloud_units: String,
    pub cloud_units_held: String,
    pub resets_at: Option<String>,
    pub period_ends_at: Option<String>,
    pub cancel_at_period_end: bool,
    pub overage_opt_in: bool,
    pub billing_url: String,
    pub usage_url: String,
}

/// Human-readable credits without floating-point rounding or loss of precision.
/// JSON snapshots retain the exact decimal string.
pub fn display_credits(value: &str) -> String {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return "—".into();
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let display_fraction = fraction[..fraction.len().min(2)].trim_end_matches('0');
    if whole == "0" && display_fraction.is_empty() && fraction.bytes().any(|b| b != b'0') {
        return "<0.01".into();
    }
    if !display_fraction.is_empty() {
        grouped.push('.');
        grouped.push_str(display_fraction);
    }
    grouped
}

impl State {
    pub fn lines(&self) -> Vec<String> {
        if !self.logged_in {
            return vec!["Sign in with `haarness login` to see your plan and credits.".into()];
        }
        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            lines.push(format!("Billing unavailable: {error}"));
            if self.summary.is_some() {
                lines.push("Last known balances (may be out of date)".into());
            }
        }
        if let Some(s) = &self.summary {
            lines.extend([
                format!("Account          {}", s.account_name),
                format!(
                    "Plan             {} · {}",
                    s.plan,
                    s.subscription_status
                        .as_deref()
                        .unwrap_or("no subscription")
                ),
                format!("Billing status   {}", s.account_status),
                String::new(),
                format!(
                    "Included AI      {} credits available",
                    display_credits(&s.included_credits)
                ),
                format!(
                    "Purchased        {} credits available",
                    display_credits(&s.purchased_credits)
                ),
                format!(
                    "Reserved         {} credits in use",
                    display_credits(&s.held_credits)
                ),
                format!(
                    "Cloud compute    {} units available ({} reserved)",
                    s.cloud_units, s.cloud_units_held
                ),
                format!(
                    "Paid overage     {}",
                    if s.overage_opt_in {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ),
            ]);
            if let Some(at) = &s.resets_at {
                lines.push(format!("Allowance reset  {at}"));
            }
            if let Some(at) = &s.period_ends_at {
                lines.push(format!(
                    "{}  {at}",
                    if s.cancel_at_period_end {
                        "Plan ends      "
                    } else {
                        "Plan renews    "
                    }
                ));
            }
            lines.extend([
                String::new(),
                format!("Manage billing   {}", s.billing_url),
                format!("Usage            {}", s.usage_url),
                "TUI: /billing open · /billing topup · /billing usage".into(),
            ]);
        } else if self.error.is_none() {
            lines.push("Loading Cloud billing…".into());
        }
        lines
    }
}

impl Client {
    pub async fn billing_summary(&self) -> Result<State> {
        let me = self.status().await?;
        if me["loggedIn"] != true {
            return Ok(State::default());
        }
        let account = me["selectedAccountId"]
            .as_str()
            .ok_or_else(|| Error::Other("Account unavailable".into()))?;
        // Validate the identifier before placing it in any URL.
        let id = account
            .parse::<uuid::Uuid>()
            .map_err(|_| Error::Other("Invalid account identifier".into()))?;
        let query = format!("?accountId={id}");
        let wallet_path = format!("api/v1/billing/wallet{query}");
        let wallet = self.request(Method::GET, &wallet_path, None);
        let subscription = async {
            match self
                .request(
                    Method::GET,
                    &format!("api/v1/billing/subscription{query}"),
                    None,
                )
                .await
            {
                Err(Error::Api { status: 404, .. }) => Ok(Value::Null),
                result => result,
            }
        };
        let (wallet, subscription) = tokio::try_join!(wallet, subscription)?;
        let text = |v: &Value, key: &str| v[key].as_str().map(str::to_owned);
        let credits = |key: &str| -> Result<String> {
            wallet[key]["credits"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::Other("Cloud returned an invalid credit balance".into()))
        };
        let base = std::env::var("HAARNESS_CLOUD_WEB_ORIGIN")
            .ok()
            .map(|url| crate::safe_url(&url))
            .transpose()?
            .unwrap_or_else(|| self.config.api_url.clone());
        let web = |path: &str| -> Result<String> {
            let mut url = base.join(path).map_err(|e| Error::Config(e.to_string()))?;
            url.query_pairs_mut().append_pair("accountId", account);
            Ok(url.to_string())
        };
        Ok(State {
            logged_in: true,
            error: None,
            summary: Some(Summary {
                account_id: account.into(),
                account_name: me["accounts"]
                    .as_array()
                    .and_then(|accounts| accounts.iter().find(|a| a["id"] == account))
                    .and_then(|a| a["name"].as_str())
                    .unwrap_or(account)
                    .into(),
                plan: text(&subscription, "planCode").unwrap_or_else(|| "No plan".into()),
                subscription_status: text(&subscription, "status"),
                account_status: text(&wallet, "status")
                    .ok_or_else(|| Error::Other("Missing billing status".into()))?,
                included_credits: credits("includedAi")?,
                purchased_credits: credits("purchased")?,
                held_credits: credits("heldReserve")?,
                cloud_units: units(&wallet, "cloudUnitSecondsAvailable")?,
                cloud_units_held: units(&wallet, "cloudUnitSecondsReserved")?,
                resets_at: text(&wallet, "resetsAt"),
                period_ends_at: text(&subscription, "currentPeriodEnd"),
                cancel_at_period_end: subscription["cancelAtPeriodEnd"].as_bool().unwrap_or(false),
                overage_opt_in: wallet["overageOptIn"].as_bool().unwrap_or(false),
                billing_url: web("app/billing")?,
                usage_url: web("app/billing/usage")?,
            }),
        })
    }
}

fn units(value: &Value, key: &str) -> Result<String> {
    let seconds = value[key]
        .as_i64()
        .ok_or_else(|| Error::Other("Missing Cloud compute balance".into()))?;
    Ok(format!("{:.2}", seconds as f64 / 3600.0))
}

struct Task(JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct Watcher {
    pub updates: watch::Receiver<State>,
    _task: Task,
}

/// Watch local account changes even while offline, so a switched account never
/// inherits another account's balance. Dropping this handle cancels all workers.
pub fn watch(client: Client) -> Watcher {
    let (sender, updates) = watch::channel(State::default());
    let task = tokio::spawn(async move {
        let mut identity = None;
        let mut worker = None;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let current = (client.config.signed_in(), client.config.selected_account());
            if identity.as_ref() == Some(&current) {
                continue;
            }
            identity = Some(current.clone());
            drop(worker.take());
            sender.send_replace(State {
                logged_in: current.0,
                ..State::default()
            });
            if current.0 {
                let client = client.clone();
                let sender = sender.clone();
                worker = Some(Task(tokio::spawn(async move {
                    snapshots(client, sender).await;
                })));
            }
        }
    });
    Watcher {
        updates,
        _task: Task(task),
    }
}

async fn snapshots(client: Client, sender: watch::Sender<State>) {
    let (hints, mut receiver) = watch::channel(false);
    let mut socket = None;
    loop {
        receiver.borrow_and_update();
        match client.billing_summary().await {
            Ok(state) => {
                if let Some(summary) = &state.summary
                    && socket.is_none()
                {
                    let client = client.clone();
                    let hints = hints.clone();
                    let account = summary.account_id.clone();
                    socket = Some(Task(tokio::spawn(async move {
                        events(client, account, hints).await;
                    })));
                }
                sender.send_if_modified(|old| {
                    if *old == state {
                        false
                    } else {
                        *old = state;
                        true
                    }
                });
            }
            Err(error) => {
                sender.send_modify(|state| {
                    if matches!(error, Error::NotLoggedIn) {
                        *state = State::default();
                    } else {
                        state.error = Some(error.to_string());
                    }
                });
            }
        }
        // Old servers and connection failures still converge; healthy streams
        // reconcile once a minute to cover dropped notifications.
        let interval = if *receiver.borrow() { 60 } else { 30 };
        tokio::select! {
            _ = receiver.changed() => { tokio::time::sleep(Duration::from_millis(150)).await; },
            () = tokio::time::sleep(Duration::from_secs(interval)) => {},
        }
    }
}

async fn events(client: Client, account: String, sender: watch::Sender<bool>) {
    let mut rejected = None;
    let mut retry = 0_u32;
    loop {
        let connected = connection(&client, &account, &sender, &mut rejected).await;
        sender.send_replace(false);
        if connected {
            retry = 0;
        }
        tokio::time::sleep(Duration::from_secs((1_u64 << retry.min(5)).min(30))).await;
        retry = retry.saturating_add(1);
    }
}

fn relevant(event: &Value, account: &str) -> bool {
    matches!(event["type"].as_str(), Some("ready" | "resync_required"))
        || (event["type"] == "entity.changed"
            && event["entity"] == "billing"
            && event["accountId"] == account)
}

async fn connection(
    client: &Client,
    account: &str,
    sender: &watch::Sender<bool>,
    rejected: &mut Option<String>,
) -> bool {
    let Ok(bearer) = client.bearer(rejected.take().as_deref()).await else {
        return false;
    };
    let Ok(mut url) = client.endpoint("api/v1/events") else {
        return false;
    };
    url.query_pairs_mut().append_pair("accountId", account);
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    if url.set_scheme(scheme).is_err() {
        return false;
    }
    let Ok(mut request) = url.as_str().into_client_request() else {
        return false;
    };
    let Ok(auth) = format!("Bearer {bearer}").parse() else {
        return false;
    };
    request.headers_mut().insert("authorization", auth);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    let mut socket = match tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
            if response.status() == reqwest::StatusCode::UNAUTHORIZED =>
        {
            *rejected = Some(bearer);
            return false;
        }
        _ => return false,
    };
    let mut ready = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(45), socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let Ok(event) = serde_json::from_str::<Value>(&text) else {
                    return ready;
                };
                if event["type"] == "ready" {
                    ready = true;
                }
                if ready && relevant(&event, account) {
                    sender.send_replace(true);
                }
            }
            Ok(Some(Ok(Message::Ping(bytes)))) => {
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Pong(bytes)))
                        .await,
                    Ok(Ok(()))
                ) {
                    return ready;
                }
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Close(Some(frame))))) if u16::from(frame.code) == 4401 => {
                *rejected = Some(bearer);
                return ready;
            }
            _ => return ready,
        }
    }
}
