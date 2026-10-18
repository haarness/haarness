//! A workspace server that cannot start says why. The detached server's
//! stderr goes nowhere, so its launcher (`serve --daemon`, and the TUI's and
//! Desktop's own launches) once reported only "exited during startup (exit
//! status: 1)".

use std::process::{Command, Stdio};

use uuid::Uuid;

#[test]
fn a_server_launched_on_a_port_in_use_says_the_port_is_in_use() {
    let root = std::env::temp_dir().join(format!("haarness-server-start-{}", Uuid::now_v7()));
    let workspace = root.join("workspace");
    let data = root.join("data");
    let home = root.join("home");
    for directory in [&workspace, &data, &home] {
        std::fs::create_dir_all(directory).expect("create fixture directory");
    }
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy a port");
    let port = occupied.local_addr().expect("occupied port").port();
    let output = Command::new(env!("CARGO_BIN_EXE_haarness"))
        .arg("--workspace")
        .arg(&workspace)
        .arg("--data-dir")
        .arg(&data)
        .args(["serve", "--daemon", "--port", &port.to_string()])
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("HAARNESS_HOME", &home)
        .env("HAARNESS_TEST_PROVIDER", "1")
        .env_remove("HAARNESS_SERVER_TOKEN")
        .env_remove("HAARNESS_SERVER_PASSWORD")
        .stdin(Stdio::null())
        .output()
        .expect("launch haarness serve --daemon");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("exited during startup"), "{stderr}");
    assert!(
        stderr.contains(&format!("Port {port} on 127.0.0.1 is already in use")),
        "{stderr}"
    );
    // The server's own log says it too, at ERROR.
    let logs = std::fs::read_dir(data.join("data/logs"))
        .expect("logs folder")
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("haarness.log")
        })
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect::<String>();
    assert!(
        logs.lines().any(|line| line.contains(" ERROR ")
            && line.contains("command failed")
            && line.contains("already in use")),
        "{logs}"
    );
    drop(occupied);
    let _ = std::fs::remove_dir_all(root);
}
