//! The agents' shell commands, which can run on in the background: the
//! routes that list, read, stop and move them, and what a published chat may
//! see of them.
use super::*;
use haarness_protocol::{BACKGROUND_COMMANDS_CAPABILITY, ShellCommandInfo, ThreadId};

async fn chat(runtime: &AgentRuntime, parent: Option<ThreadId>) -> ThreadId {
    runtime
        .create_thread(CreateThreadRequest {
            parent_id: parent,
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn the_command_routes_answer_for_a_chat_and_stay_inside_a_published_chat() {
    let workspace = std::env::temp_dir();
    let (runtime, _) = p0_runtime(&workspace, true);
    let runtime = Arc::new(runtime);
    let handle = activity_server(runtime.clone(), None, &workspace).await;
    let base = format!("http://{}", handle.address);
    let thread = chat(&runtime, None).await;
    let child = chat(&runtime, Some(thread)).await;
    let other = chat(&runtime, None).await;
    let client = reqwest::Client::new();
    let send = |method: reqwest::Method, path: String, scope: Option<ThreadId>| {
        let mut request = client
            .request(method, format!("{base}/v1/{path}"))
            .bearer_auth("activity-token");
        if let Some(scope) = scope {
            request = request.header("x-haarness-remote-thread", scope.to_string());
        }
        request.send()
    };
    let get = reqwest::Method::GET;
    let post = reqwest::Method::POST;

    let info: Value = send(get.clone(), "info".into(), None)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == BACKGROUND_COMMANDS_CAPABILITY),
        "{info}"
    );
    // A chat with no commands lists none, and so does every chat.
    for path in [
        format!("threads/{thread}/commands"),
        format!("threads/{child}/commands"),
        "commands/active".to_owned(),
    ] {
        let response = send(get.clone(), path.clone(), None).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        let listed: Vec<ShellCommandInfo> = response.json().await.unwrap();
        assert!(listed.is_empty(), "{path}");
    }
    // An unknown command, or one of an unknown chat, is not found.
    for (method, path) in [
        (get.clone(), format!("threads/{thread}/commands/c-missing")),
        (
            get.clone(),
            format!("threads/{thread}/commands/c-missing/output?cursor=0&limit=10&tail=true"),
        ),
        (
            post.clone(),
            format!("threads/{thread}/commands/c-missing/stop"),
        ),
        (
            post.clone(),
            format!("threads/{thread}/commands/background?commandId=c-missing"),
        ),
        (get.clone(), format!("threads/{}/commands", ThreadId::new())),
    ] {
        let response = send(method, path.clone(), None).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND, "{path}");
    }
    // Moving every command of a chat with none moves nothing.
    let moved: Vec<ShellCommandInfo> = send(
        post.clone(),
        format!("threads/{thread}/commands/background"),
        None,
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(moved.is_empty());
    // Slash commands still run on the same path with POST.
    let command = client
        .post(format!("{base}/v1/threads/{thread}/commands"))
        .bearer_auth("activity-token")
        .json(&serde_json::json!({"name": "no-such-command"}))
        .send()
        .await
        .unwrap();
    assert_ne!(command.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    assert!(command.status().is_client_error(), "{}", command.status());
    let activity: Value = send(get.clone(), format!("threads/{thread}/activity"), None)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(activity["backgroundCommands"], 0, "{activity}");

    // A published chat reads its own tree's commands and every chat's
    // running ones (filtered to its tree), never another chat's.
    for path in [
        format!("threads/{thread}/commands"),
        format!("threads/{child}/commands"),
        "commands/active".to_owned(),
    ] {
        let response = send(get.clone(), path.clone(), Some(thread)).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
    }
    for (method, path) in [
        (get.clone(), format!("threads/{other}/commands")),
        (post.clone(), format!("threads/{other}/commands/background")),
        (
            post.clone(),
            format!("threads/{other}/commands/c-missing/stop"),
        ),
    ] {
        let response = send(method, path.clone(), Some(thread)).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN, "{path}");
    }
    handle.shutdown().await.unwrap();
}
