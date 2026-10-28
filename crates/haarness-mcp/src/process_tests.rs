use super::*;

fn alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && !String::from_utf8_lossy(&output.stdout)
                    .trim_start()
                    .starts_with('Z')
        })
}

async fn gone(log: &Path) {
    let pids = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| line.parse::<u32>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(pids.len(), 3, "fixture must include a detached grandchild");
    tokio::time::timeout(Duration::from_secs(8), async {
        while pids.iter().any(|pid| alive(*pid)) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("MCP descendants survived cleanup");
}

fn config(log: &Path, delay: &str) -> McpConfigureRequest {
    serde_json::from_value(serde_json::json!({
        "transport":"stdio", "command":"python3",
        "args":[concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/process_tree.py")],
        "env":{"TREE_LOG":log, "INIT_DELAY":delay}, "timeoutMs":5000, "enabled":true,
    }))
    .unwrap()
}

#[tokio::test]
async fn disconnect_reconnect_and_failed_handshakes_reap_descendants_only() {
    let root = std::env::temp_dir().join(format!("haarness-mcp-tree-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let mut sentinel = tokio::process::Command::new("sleep")
        .arg("45")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let registry = NativeMcpRegistry::default();
    let first = root.join("first");
    registry
        .configure("tree", config(&first, "0"))
        .await
        .unwrap();
    let previous = std::fs::read_to_string(&first).unwrap();
    registry.reconnect("tree").await.unwrap();
    // Reconnection starts a new fixture at the same path, so retain the first PIDs.
    let old = root.join("old");
    std::fs::write(&old, previous).unwrap();
    gone(&old).await;
    registry.disconnect("tree").await.unwrap();
    gone(&first).await;
    let timeout = root.join("timeout");
    assert!(
        registry
            .configure("tree", config(&timeout, "30"))
            .await
            .is_err()
    );
    gone(&timeout).await;
    assert!(
        sentinel.try_wait().unwrap().is_none(),
        "cleanup killed an unrelated process"
    );
    sentinel.kill().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Waits until the fixture logged its three processes in `log`.
async fn launched(log: &Path) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while std::fs::read_to_string(log).map_or(0, |text| text.lines().count()) < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the fixture did not start");
}

/// Waits until every process the fixture logged in `log` has exited, at
/// most `limit`.
async fn gone_within(log: &Path, limit: Duration) {
    let pids = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| line.parse::<u32>().unwrap())
        .collect::<Vec<_>>();
    tokio::time::timeout(limit, async {
        while pids.iter().any(|pid| alive(*pid)) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("a stopped start left its processes running");
}

/// A server that answers `initialize` only after its start would have
/// timed out, with a minute to start.
fn hung(log: &Path) -> McpConfigureRequest {
    let mut config = config(log, "30");
    config.timeout_ms = Some(60_000);
    config
}

/// A start that is no longer wanted stops the processes it began rather
/// than running to its end: its server disconnected, configured again by a
/// reload, a reload never applied, or Haarness stopping, which also ends the
/// waits for it.
#[tokio::test]
async fn a_start_no_longer_wanted_stops_its_processes() {
    let root = std::env::temp_dir().join(format!("haarness-mcp-stop-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let mut sentinel = tokio::process::Command::new("sleep")
        .arg("45")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let limit = Duration::from_secs(3);
    let registry = NativeMcpRegistry::default();
    let disconnected = root.join("disconnected");
    registry.prepare("tree", hung(&disconnected)).unwrap();
    launched(&disconnected).await;
    registry.disconnect("tree").await.unwrap();
    gone_within(&disconnected, limit).await;

    let replaced = root.join("replaced");
    registry.prepare("tree", hung(&replaced)).unwrap();
    launched(&replaced).await;
    let candidate = NativeMcpRegistry::default();
    let replacement = root.join("replacement");
    candidate
        .prepare_reusing(&registry, "tree", hung(&replacement))
        .unwrap();
    registry.replace_from(&candidate);
    drop(candidate);
    gone_within(&replaced, limit).await;
    launched(&replacement).await;

    let unapplied = root.join("unapplied");
    let candidate = NativeMcpRegistry::default();
    candidate.prepare("other", hung(&unapplied)).unwrap();
    launched(&unapplied).await;
    drop(candidate);
    gone_within(&unapplied, limit).await;

    let waiting = registry.starting().expect("a turn waits for the start");
    let request = tokio::spawn({
        let registry = registry.clone();
        async move { registry.tools("tree").await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    registry.close();
    tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("stopping ended a turn's wait");
    tokio::time::timeout(Duration::from_secs(1), request)
        .await
        .expect("stopping ended a request's wait")
        .unwrap()
        .unwrap();
    gone_within(&replacement, limit).await;
    assert!(
        sentinel.try_wait().unwrap().is_none(),
        "stopping a start killed an unrelated process"
    );
    sentinel.kill().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
