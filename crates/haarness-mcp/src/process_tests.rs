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
