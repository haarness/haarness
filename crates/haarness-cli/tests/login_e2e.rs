//! `haarness login` end to end against a mock Haarness Cloud: the token
//! request names the sign-in after this computer, and after Desktop when
//! Desktop runs the command (`apps/desktop/src/main/account.ts`).
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Form, Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, time::timeout};
use uuid::Uuid;

type Forms = Arc<Mutex<Vec<HashMap<String, String>>>>;

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs a device sign-in the way Desktop does and returns the label Cloud
/// received.
async fn device_label(desktop: bool) -> String {
    let forms = Forms::default();
    let cloud = Router::new()
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
            post(|State(forms): State<Forms>, Form(form): Form<HashMap<String, String>>| async move {
                forms.lock().unwrap().push(form);
                Json(json!({"access_token": "access", "refresh_token": "refresh", "expires_in": 600}))
            }),
        )
        .route(
            "/api/v1/me",
            get(|| async {
                Json(json!({"user": {"email": "e2e@example.test"}, "accounts": [],
                    "defaultAccountId": "a"}))
            }),
        )
        .with_state(forms.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async { axum::serve(listener, cloud).await.unwrap() });
    let directory =
        TestDirectory(std::env::temp_dir().join(format!("haarness-login-e2e-{}", Uuid::now_v7())));
    let mut command = Command::new(env!("CARGO_BIN_EXE_haarness"));
    command
        .args(["login", "--json", "--no-browser", "--device"])
        .current_dir(std::env::temp_dir())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory.0.join("home"))
        .env("HAARNESS_DATA_DIR", directory.0.join("data"))
        .env("HAARNESS_CLOUD_CONFIG_DIR", directory.0.join("account"))
        .env("HAARNESS_CLOUD_API_URL", format!("http://{address}"))
        .env("HOSTNAME", "e2e-box")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if desktop {
        command.env("HAARNESS_LOGIN_APP", "desktop");
    }
    let output = timeout(Duration::from_secs(60), command.output())
        .await
        .expect("login finished within a minute")
        .expect("run haarness login");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let complete: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(complete["type"], "complete", "{stdout}");
    server.abort();
    let forms = forms.lock().unwrap();
    assert_eq!(forms.len(), 1, "{forms:?}");
    assert_eq!(
        forms[0]["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    forms[0]["device_label"].clone()
}

#[tokio::test]
async fn login_names_the_sign_in_after_this_computer_and_the_app() {
    // macOS names the computer itself (`scutil`); elsewhere it is the host name.
    let computer = if cfg!(target_os = "macos") {
        haarness_account::computer_name()
    } else {
        "e2e-box".to_owned()
    };
    assert_eq!(
        device_label(false).await,
        haarness_account::device_label(false, &computer)
    );
    assert_eq!(
        device_label(true).await,
        haarness_account::device_label(true, &computer)
    );
}
